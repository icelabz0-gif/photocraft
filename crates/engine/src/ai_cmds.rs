//! AI-assisted inline editing commands.
//!
//! Commands:
//! - `ai.removeObject`  – fill a selection with AI-generated content
//!   (inpainting / object removal); falls back to classical PatchMatch.
//! - `ai.skinEnhance`   – detect skin via segmentation, apply frequency-
//!   separation smoothing; falls back to a local Gaussian smooth.
//! - `ai.pingBackend`   – liveness check used by Preferences › AI (not journalled).
//!
//! All commands:
//! - Return `Err` on bad params or backend failure; never panic.
//! - Do not modify the document on failure.
//! - Dispatch inference to the configured local backend (Ollama, llama.cpp).
//! - Fall back to classical algorithms when the backend is unavailable or
//!   returns `MlError::Unsupported`.

use photocraft_algo::retouch::local_blur;
use photocraft_doc::{Layer, LayerContent, LayerId};
use photocraft_geom::Rect;
use photocraft_ml::{InferenceBackend, InferenceRequest, InferenceResult, LocalBackend, MlError, Task};
use photocraft_raster::Surface;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

// ─────────────────────────────────────────────────────────────────────────────
// Command registry
// ─────────────────────────────────────────────────────────────────────────────

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "ai.removeObject",
            label: "Remove Object",
            menu: &["Edit", "Content-Aware"],
            shortcut: None,
            params: r#"{
  "layer":    integer (optional, defaults to active),
  "prompt":   string  (optional fill hint for the model),
  "seed":     integer (optional, for reproducibility),
  "strength": number  0.0–1.0 (default 0.85, how aggressively the model replaces),
  "output":   "newLayer" | "inPlace" (default "newLayer")
}"#,
            enabled: has_pixel_layer,
            run: remove_object,
            journal: true,
        },
        CommandSpec {
            id: "ai.skinEnhance",
            label: "Skin Enhance",
            menu: &["Filter", "Neural Filters"],
            shortcut: None,
            params: r#"{
  "layer":          integer (optional, defaults to active),
  "strength":       number  0–100 (default 50),
  "smoothRadius":   integer 2–20  (default 6),
  "preserveDetail": bool    (default true)
}"#,
            enabled: has_pixel_layer,
            run: skin_enhance,
            journal: true,
        },
        CommandSpec {
            id: "ai.pingBackend",
            label: "Test AI Connection",
            menu: &[],
            shortcut: None,
            params: r#"{}"#,
            enabled: crate::commands::always,
            run: ping_backend,
            journal: false,
        },
    ]
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared helpers
// ─────────────────────────────────────────────────────────────────────────────

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn has_pixel_layer(s: &Session) -> std::result::Result<(), String> {
    let l = crate::active_layer_of(s)?;
    if matches!(l.content, LayerContent::Raster(_)) {
        Ok(())
    } else {
        Err(format!("active layer is {} {} layer, not a pixel layer", l.content.article(), l.content.kind_name()))
    }
}

fn num(p: &Value, k: &str, d: f64) -> f64 {
    p.get(k).and_then(Value::as_f64).unwrap_or(d)
}
fn flag(p: &Value, k: &str, d: bool) -> bool {
    p.get(k).and_then(Value::as_bool).unwrap_or(d)
}
fn str_param<'a>(p: &'a Value, k: &str, d: &'a str) -> &'a str {
    p.get(k).and_then(Value::as_str).unwrap_or(d)
}

fn make_backend(s: &Session) -> Box<dyn InferenceBackend> {
    let ai = &s.prefs().ai;
    Box::new(LocalBackend::new(
        ai.local_url.clone(),
        ai.local_model.clone(),
        ai.local_timeout_secs,
    ))
}

fn check_region_size(s: &Session, cmd: &str, rect: Rect) -> Result<()> {
    let cap = s.prefs().ai.max_region_mpx;
    let mp = rect.width() as f32 * rect.height() as f32 / 1_000_000.0;
    if mp > cap {
        return Err(bad(cmd, format!("region is {mp:.1} MP, exceeds the {cap:.1} MP AI cap (Edit › Preferences › AI)")));
    }
    Ok(())
}

fn resolve_layer_id(s: &Session, p: &Value, cmd: &str) -> Result<LayerId> {
    match p.get("layer").and_then(Value::as_u64) {
        Some(id) => Ok(LayerId(id)),
        None => s.active().and_then(|d| d.active_layer).ok_or(EngineError::Other("no active layer".into())),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ai.removeObject
// ─────────────────────────────────────────────────────────────────────────────

fn remove_object(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "ai.removeObject";

    let id = resolve_layer_id(s, p, CMD)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let doc_bounds = st.doc.bounds();
    let sel = st.doc.selection.clone().ok_or_else(|| bad(CMD, "no active selection — make a selection first"))?;
    let bounds = sel.content_bounds().intersect(&doc_bounds);
    if bounds.is_empty() {
        return Err(bad(CMD, "selection is empty or outside the canvas"));
    }
    check_region_size(s, CMD, bounds)?;

    let layer = st.doc.layer(id).ok_or_else(|| bad(CMD, format!("layer {id:?} not found")))?;
    let surf = layer.surface().ok_or_else(|| bad(CMD, "layer has no pixel surface"))?.clone();

    let prompt = p.get("prompt").and_then(Value::as_str).map(str::to_owned);
    let seed = p.get("seed").and_then(Value::as_u64);
    let strength = num(p, "strength", 0.85).clamp(0.0, 1.0) as f32;
    let output_mode = str_param(p, "output", "newLayer");
    if !matches!(output_mode, "newLayer" | "inPlace") {
        return Err(bad(CMD, format!("unknown output `{output_mode}` (newLayer|inPlace)")));
    }

    let backend = make_backend(s);
    let req = InferenceRequest {
        task: Task::Inpaint,
        image: surf.clone(),
        mask: Some(sel.clone()),
        prompt,
        negative_prompt: None,
        seed,
        strength,
        scale: 1,
        region: bounds,
    };

    let result_surface = match backend.run(req) {
        Ok(InferenceResult::Pixels(res)) => res,
        Ok(InferenceResult::Mask(_)) => {
            return Err(bad(CMD, "backend returned a mask instead of pixels"));
        }
        Err(MlError::Unsupported) | Err(MlError::Network(_)) => {
            // Fall back to classical PatchMatch inpaint via content_aware_native.
            let (window, filled) = crate::fill_cmds::content_aware_native(
                &surf,
                bounds,
                doc_bounds,
                true,
                seed.unwrap_or(1),
                |x, y| sel.sample_channel(x, y, 0) > 0.0,
                |_, _| true,
            )?;
            let mut out = Surface::new(surf.format());
            out.write_region(window, &filled);
            out
        }
        Err(e) => return Err(EngineError::Other(format!("AI backend error: {e}"))),
    };

    let dmg = if output_mode == "inPlace" {
        commit_inplace(s, id, &result_surface, bounds, "Remove Object")?
    } else {
        commit_new_layer(s, id, result_surface, bounds, "Remove Object", "AI: Remove Object")?
    };

    Ok(json!({ "damage": [dmg.x0, dmg.y0, dmg.width(), dmg.height()] }))
}

// ─────────────────────────────────────────────────────────────────────────────
// ai.skinEnhance
// ─────────────────────────────────────────────────────────────────────────────

fn skin_enhance(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "ai.skinEnhance";

    let id = resolve_layer_id(s, p, CMD)?;
    let strength = num(p, "strength", 50.0).clamp(0.0, 100.0) as f32 / 100.0;
    let smooth_radius = num(p, "smoothRadius", 6.0).clamp(2.0, 20.0) as f32;
    let preserve_detail = flag(p, "preserveDetail", true);

    let st = s.active().ok_or(EngineError::NoDocument)?;
    let layer = st.doc.layer(id).ok_or_else(|| bad(CMD, "layer not found"))?;
    let surf = layer.surface().ok_or_else(|| bad(CMD, "layer has no pixel surface"))?.clone();
    let bounds = surf.content_bounds().intersect(&st.doc.bounds());
    if bounds.is_empty() {
        return Ok(json!({ "damage": [0, 0, 0, 0] }));
    }
    check_region_size(s, CMD, bounds)?;

    let backend = make_backend(s);
    let skin_mask = if backend.supports(Task::SkinSegment) {
        let req = InferenceRequest {
            task: Task::SkinSegment,
            image: surf.clone(),
            mask: None,
            prompt: None,
            negative_prompt: None,
            seed: None,
            strength: 1.0,
            scale: 1,
            region: bounds,
        };
        match backend.run(req) {
            Ok(InferenceResult::Mask(m)) => Some(m),
            _ => None,
        }
    } else {
        None
    };

    let skin_mask = skin_mask.unwrap_or_else(|| hsv_skin_mask(&surf, bounds));
    let result = freq_sep_smooth(&surf, &skin_mask, smooth_radius, strength, preserve_detail, bounds)?;

    let dmg = commit_inplace(s, id, &result, bounds, "Skin Enhance")?;
    Ok(json!({ "damage": [dmg.x0, dmg.y0, dmg.width(), dmg.height()] }))
}

fn hsv_skin_mask(image: &Surface, bounds: Rect) -> Surface {
    let (w, h_px) = (bounds.width() as usize, bounds.height() as usize);
    let mut rgba_buf = vec![[0.0f32; 4]; w * h_px];
    image.read_rgba_into(bounds, &mut rgba_buf);

    let mut mask = vec![0.0f32; w * h_px];
    for (i, px) in rgba_buf.iter().enumerate() {
        let (r, g, b) = (px[0], px[1], px[2]);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;
        let v = max;
        let s = if max > 0.0 { delta / max } else { 0.0 };
        let h = if delta < 1e-6 {
            0.0f32
        } else if (max - r).abs() < 1e-6 {
            60.0 * (((g - b) / delta) % 6.0)
        } else if (max - g).abs() < 1e-6 {
            60.0 * ((b - r) / delta + 2.0)
        } else {
            60.0 * ((r - g) / delta + 4.0)
        };
        let h = if h < 0.0 { h + 360.0 } else { h };
        let is_skin = (h <= 50.0 || h >= 330.0) && (0.15..=0.90).contains(&s) && (0.20..=0.95).contains(&v);
        mask[i] = if is_skin { 1.0 } else { 0.0 };
    }

    let mut out = Surface::new(photocraft_color::PixelFormat::GRAY32F);
    out.write_region(bounds, &mask);
    out
}

fn freq_sep_smooth(
    image: &Surface,
    mask: &Surface,
    radius: f32,
    strength: f32,
    preserve_detail: bool,
    bounds: Rect,
) -> Result<Surface> {
    let (w, h) = (bounds.width() as usize, bounds.height() as usize);
    let ch = image.format().channels();
    let data = image.read_region(bounds);
    let mask_data = mask.read_region(bounds);

    let alpha_idx = if image.format().alpha { Some(ch - 1) } else { None };
    let low = local_blur(&data, w, h, ch, alpha_idx, (0, 0, w, h), radius);

    let mut result = data.clone();
    for i in 0..(w * h) {
        let m = mask_data.get(i).copied().unwrap_or(0.0) * strength;
        if m <= 0.0 {
            continue;
        }
        for c in 0..ch {
            if Some(c) == alpha_idx {
                continue;
            }
            let orig = data[i * ch + c];
            let lo = low.get(i * ch + c).copied().unwrap_or(orig);
            let smoothed = if preserve_detail {
                let hi = orig - lo;
                lo + hi * (1.0 - m)
            } else {
                lo
            };
            result[i * ch + c] = orig * (1.0 - m) + smoothed * m;
        }
    }

    let mut out = Surface::new(image.format());
    out.write_region(bounds, &result);
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────────
// ai.pingBackend
// ─────────────────────────────────────────────────────────────────────────────

fn ping_backend(s: &mut Session, _p: &Value) -> Result<Value> {
    let backend = make_backend(s);
    match backend.ping() {
        Ok(()) => Ok(json!({ "ok": true, "backend": backend.id() })),
        Err(MlError::Unsupported) => Ok(json!({ "ok": false, "reason": "not available in this build (wasm)" })),
        Err(e) => Ok(json!({ "ok": false, "reason": e.to_string() })),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Commit helpers
// ─────────────────────────────────────────────────────────────────────────────

fn commit_inplace(s: &mut Session, id: LayerId, result: &Surface, bounds: Rect, label: &str) -> Result<Rect> {
    s.edit(label, |doc, _| {
        let layer = doc.layer_mut(id).ok_or_else(|| EngineError::Other("layer vanished".into()))?;
        let surf = layer.surface_mut().ok_or_else(|| EngineError::Other("no pixel surface".into()))?;
        let converted = if result.format() == surf.format() {
            result.clone()
        } else {
            result.convert(surf.format())
        };
        surf.write_region(bounds, &converted.read_region(bounds));
        Ok(bounds)
    })
}

fn commit_new_layer(
    s: &mut Session,
    above_id: LayerId,
    result: Surface,
    bounds: Rect,
    label: &str,
    layer_name: &str,
) -> Result<Rect> {
    s.edit(label, |doc, active| {
        let name = doc.next_layer_name(layer_name);
        let layer = Layer::new(name, LayerContent::Raster(result));
        let new_id = doc.insert_above(Some(above_id), layer);
        *active = Some(new_id);
        Ok(bounds)
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_session() -> Session {
        Session::headless_for_test()
    }

    #[test]
    fn remove_object_no_document_returns_err() {
        let mut s = make_session();
        let r = remove_object(&mut s, &json!({}));
        assert!(r.is_err(), "expected Err with no document");
    }

    #[test]
    fn remove_object_bad_output_param_returns_err() {
        let mut s = make_session();
        let r = remove_object(&mut s, &json!({"output": "magic"}));
        assert!(r.is_err());
    }

    #[test]
    fn skin_enhance_no_document_returns_err() {
        let mut s = make_session();
        let r = skin_enhance(&mut s, &json!({}));
        assert!(r.is_err());
    }

    #[test]
    fn ping_returns_ok_shape() {
        let mut s = make_session();
        let r = ping_backend(&mut s, &json!({}));
        assert!(r.is_ok());
        let v = r.expect("ping should not panic");
        assert!(v.get("ok").is_some(), "response must have `ok` field");
    }
}
