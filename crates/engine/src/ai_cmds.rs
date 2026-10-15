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
use crate::jobs;
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
            params: r#"{"prompt":text,"seed":u32=0,"strength":0.0..1.0=0.85,"output":"newLayer|inPlace"="newLayer"}"#,
            enabled: has_pixel_layer,
            run: remove_object,
            journal: true,
        },
        CommandSpec {
            id: "ai.skinEnhance",
            label: "Skin Enhance",
            menu: &["Filter", "Neural Filters"],
            shortcut: None,
            params: r#"{"strength":0..100=50,"smoothRadius":2..20=6,"preserveDetail":bool=true}"#,
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

    let new_layer = output_mode == "newLayer";

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

    // Heavy work on the job worker (progress, Esc cancels, the document stays
    // unchanged); the commit runs on the caller thread through the edit path.
    jobs::run(
        s,
        "Remove Object",
        true,
        move |ctx| {
            ctx.progress(0.0, "Encoding");
            ctx.check()?;
            let generated = match backend.run(req) {
                Ok(InferenceResult::Pixels(res)) => Some(res),
                Ok(InferenceResult::Mask(_)) => {
                    return Err(bad(CMD, "backend returned a mask instead of pixels"));
                }
                // Fall back to classical PatchMatch inpaint via content_aware_native.
                Err(MlError::Unsupported) | Err(MlError::Network(_)) => None,
                Err(e) => return Err(EngineError::Other(format!("AI backend error: {e}"))),
            };
            ctx.progress(0.9, "Refining");
            ctx.check()?;
            match generated {
                Some(res) => Ok(res),
                None => {
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
                    Ok(out)
                }
            }
        },
        move |s, result| {
            let dmg = if new_layer {
                commit_new_layer(s, id, result, bounds, "Remove Object", "AI: Remove Object")?
            } else {
                commit_inplace(s, id, &result, bounds, "Remove Object")?
            };
            Ok(json!({ "damage": [dmg.x0, dmg.y0, dmg.width(), dmg.height()] }))
        },
    )
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
    jobs::run(
        s,
        "Skin Enhance",
        true,
        move |ctx| {
            ctx.progress(0.0, "Detecting skin");
            ctx.check()?;
            let segmented = if backend.supports(Task::SkinSegment) {
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
            ctx.check()?;
            let skin_mask = segmented.unwrap_or_else(|| hsv_skin_mask(&surf, bounds));
            ctx.progress(0.4, "Smoothing");
            let result = freq_sep_smooth(&surf, &skin_mask, smooth_radius, strength, preserve_detail, bounds)?;
            ctx.progress(0.95, "");
            Ok(result)
        },
        move |s, result| {
            let dmg = commit_inplace(s, id, &result, bounds, "Skin Enhance")?;
            Ok(json!({ "damage": [dmg.x0, dmg.y0, dmg.width(), dmg.height()] }))
        },
    )
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
    use crate::jobs::{JobCtx, JobOutcome, Started};
    use serde_json::json;
    use std::time::{Duration, Instant};

    fn make_session() -> Session {
        Session::headless_for_test()
    }

    /// A document with a textured background layer, a selection in its middle
    /// and the AI backend pointed at a port nothing listens on, so the
    /// classical fallback runs at once.
    fn session_with_selection(w: u32, h: u32) -> Session {
        let mut s = make_session();
        s.execute("file.new", json!({"width": w, "height": h})).unwrap();
        s.edit("paint", |doc, _| {
            let surf = doc.layers[0].surface_mut().unwrap();
            let r = photocraft_geom::Rect::new(0, 0, w as i32, h as i32);
            let mut px = Vec::with_capacity((w * h * 4) as usize);
            for y in 0..h {
                for x in 0..w {
                    let v = ((x * 7 + y * 13) % 31) as f32 / 30.0;
                    px.extend_from_slice(&[v, 1.0 - v, ((x / 5 + y / 3) % 2) as f32, 1.0]);
                }
            }
            surf.write_region(r, &px);
            let mut sel = Surface::new(photocraft_color::PixelFormat::GRAY8);
            sel.fill_rect(photocraft_geom::Rect::new(8, 8, 24, 24), &[1.0]);
            doc.selection = Some(sel);
            Ok(())
        })
        .unwrap();
        s.prefs.edit(|p| p.ai.local_url = "http://127.0.0.1:9".into());
        s
    }

    fn pixels(s: &Session) -> Vec<f32> {
        let d = s.active().unwrap();
        let l = d.doc.layer(d.active_layer.unwrap()).unwrap();
        l.surface().unwrap().read_region(d.doc.bounds())
    }

    /// Poll until job `id` ends (or 60 s pass); returns its event.
    fn wait_event(s: &mut Session, id: crate::jobs::JobId) -> crate::jobs::JobEvent {
        let t = Instant::now();
        loop {
            if let Some(e) = s.poll_jobs().into_iter().find(|e| e.id == id) {
                return e;
            }
            assert!(t.elapsed() < Duration::from_secs(60), "job {id:?} did not finish");
            std::thread::sleep(Duration::from_millis(2));
        }
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

    #[test]
    fn remove_object_runs_as_a_background_job() {
        let mut s = session_with_selection(48, 48);
        let layers = s.active().unwrap().doc.layers.len();
        let Started::Job(id) = s.start("ai.removeObject", json!({})).unwrap() else {
            panic!("ai.removeObject must run as a background job");
        };
        let e = wait_event(&mut s, id);
        assert!(matches!(e.outcome, JobOutcome::Done(_)), "{e:?}");
        let st = s.active().unwrap();
        assert_eq!(st.doc.layers.len(), layers + 1, "the AI result landed as a new layer");
        assert_eq!(st.doc.layers.last().unwrap().name, "AI: Remove Object");
        assert!(st.doc.selection.is_some(), "the selection survives");
    }

    #[test]
    fn skin_enhance_cancelled_leaves_the_document_unchanged() {
        let mut s = session_with_selection(48, 48);
        let before = pixels(&s);
        let ctx = JobCtx::new();
        ctx.cancel();
        s.set_inline_job_ctx(Some(ctx));
        let r = s.execute("ai.skinEnhance", json!({}));
        s.set_inline_job_ctx(None);
        assert!(matches!(r, Err(EngineError::Cancelled)), "{r:?}");
        assert_eq!(pixels(&s), before, "a cancelled run must not touch the pixels");
    }

    #[test]
    fn skin_enhance_reports_progress_and_completes_inline() {
        let mut s = session_with_selection(48, 48);
        let before = pixels(&s);
        let ctx = JobCtx::new();
        s.set_inline_job_ctx(Some(ctx.clone()));
        s.execute("ai.skinEnhance", json!({"strength": 80})).unwrap();
        s.set_inline_job_ctx(None);
        assert!(ctx.fraction() >= 0.95, "progress reported, got {}", ctx.fraction());
        assert_ne!(pixels(&s), before, "smoothing changed the pixels");
    }
}
