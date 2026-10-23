//! Filter › Darkroom and Filter › Glitch: filters ported from Photoslop (Apache-2.0, same author)
//! that Photoshop's menu doesn't have: Sepia, Chroma Denoise, Film Negative → Positive, Pixel Sort,
//! Datamosh + Chromatic Aberration, Retro Console and Beam Dither.
//!
//! They run through the shared filter plumbing ([`crate::filters::run_filter`]): selection-aware,
//! undoable, recorded as smart filters on smart objects, replayed by Last Filter, and the dialog
//! is generated from the params spec. The algorithms live in `photocraft_algo` (`darkroom`,
//! `glitch`).

use photocraft_algo::{BeamDitherSpec, BeamMode, DitherAlgorithm, FilterParams, NegativeMode};
use serde_json::Value;

use crate::commands::CommandSpec;

fn f(p: &Value, k: &str, d: f32) -> f32 {
    p.get(k).and_then(Value::as_f64).map_or(d, |v| v as f32)
}
fn i(p: &Value, k: &str, d: i64) -> i64 {
    p.get(k).and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f.round().clamp(i64::MIN as f64, i64::MAX as f64) as i64))).unwrap_or(d)
}
fn b(p: &Value, k: &str, d: bool) -> bool {
    p.get(k).and_then(Value::as_bool).unwrap_or(d)
}
fn s<'a>(p: &'a Value, k: &str, d: &'a str) -> &'a str {
    p.get(k).and_then(Value::as_str).unwrap_or(d)
}
/// A colour as `[r, g, b(, a)]` 0–1 or `#RRGGBB` (what the dialog's colour wells produce).
fn colour(p: &Value, k: &str, d: [f32; 4]) -> [f32; 4] {
    match p.get(k) {
        Some(Value::Array(a)) if a.len() >= 3 => {
            let c = |j: usize, dv: f32| a.get(j).and_then(Value::as_f64).map_or(dv, |v| (v as f32).clamp(0.0, 1.0));
            [c(0, 0.0), c(1, 0.0), c(2, 0.0), c(3, 1.0)]
        }
        Some(Value::String(h)) if h.len() == 7 && h.starts_with('#') && h.is_char_boundary(1) => {
            let ch = |j: usize| h.get(j..j + 2).and_then(|s| u8::from_str_radix(s, 16).ok()).map_or(0.0, |v| f32::from(v) / 255.0);
            [ch(1), ch(3), ch(5), 1.0]
        }
        _ => d,
    }
}

const DARKROOM: &[&str] = &["Filter", "Darkroom"];
const GLITCH: &[&str] = &["Filter", "Glitch"];

/// The command ids of this module.
pub const IDS: [&str; 7] = [
    "filter.darkroom.sepia",
    "filter.darkroom.chromaDenoise",
    "filter.darkroom.filmNegative",
    "filter.glitch.pixelSort",
    "filter.glitch.datamosh",
    "filter.glitch.retroConsole",
    "filter.glitch.beamDither",
];

/// Algorithm parameters for one of [`IDS`] from JSON params (dialog units). `None` otherwise.
pub fn params_for(id: &str, p: &Value) -> Option<FilterParams> {
    Some(match id {
        "filter.darkroom.sepia" => FilterParams::Sepia { amount: f(p, "amount", 80.0).clamp(0.0, 100.0) },
        "filter.darkroom.chromaDenoise" => FilterParams::ChromaDenoise { strength: f(p, "strength", 40.0).clamp(1.0, 100.0) },
        "filter.darkroom.filmNegative" => FilterParams::FilmNegative {
            mode: match s(p, "mode", "auto") {
                "color" => NegativeMode::Color,
                "mono" => NegativeMode::Mono,
                _ => NegativeMode::Auto,
            },
            clip: f(p, "clip", 0.5).clamp(0.0, 5.0),
        },
        "filter.glitch.pixelSort" => FilterParams::PixelSort {
            low: f(p, "low", 60.0).clamp(0.0, 255.0),
            high: f(p, "high", 200.0).clamp(0.0, 255.0),
            vertical: s(p, "direction", "rows") == "columns",
            reverse: s(p, "order", "darkFirst") == "brightFirst",
        },
        "filter.glitch.datamosh" => FilterParams::Datamosh {
            block: i(p, "block", 16).clamp(4, 64) as u32,
            amount: f(p, "amount", 35.0).clamp(0.0, 100.0) / 100.0,
            drift: i(p, "drift", 12).clamp(0, 64) as u32,
            aberration: f(p, "aberration", 3.0).clamp(0.0, 20.0),
            seed: i(p, "seed", 7).clamp(0, i64::from(u32::MAX)) as u32,
        },
        "filter.glitch.retroConsole" => {
            FilterParams::RetroConsole { size: i(p, "size", 6).clamp(1, 64) as u32, levels: i(p, "levels", 4).clamp(2, 8) as u32, dither: b(p, "dither", true) }
        }
        "filter.glitch.beamDither" => FilterParams::BeamDither {
            spec: BeamDitherSpec {
                algorithm: match s(p, "algorithm", "beam") {
                    "floydSteinberg" => DitherAlgorithm::FloydSteinberg,
                    "atkinson" => DitherAlgorithm::Atkinson,
                    "jarvis" => DitherAlgorithm::Jarvis,
                    "stucki" => DitherAlgorithm::Stucki,
                    "sierra" => DitherAlgorithm::Sierra,
                    "burkes" => DitherAlgorithm::Burkes,
                    "bayer2" => DitherAlgorithm::Bayer2,
                    "bayer4" => DitherAlgorithm::Bayer4,
                    "bayer8" => DitherAlgorithm::Bayer8,
                    "threshold" => DitherAlgorithm::Threshold,
                    _ => DitherAlgorithm::Beam,
                },
                mode: match s(p, "mode", "mono") {
                    "tonal" => BeamMode::Tonal,
                    "color" => BeamMode::Color,
                    _ => BeamMode::Mono,
                },
                scale: i(p, "scale", 3).clamp(1, 32) as u32,
                levels: i(p, "levels", 2).clamp(2, 8) as u32,
                brightness: f(p, "brightness", 0.0).clamp(-100.0, 100.0),
                contrast: f(p, "contrast", 0.0).clamp(-100.0, 100.0),
                blur: f(p, "blur", 0.0).clamp(0.0, 20.0),
                sharpen: f(p, "sharpen", 0.0).clamp(0.0, 300.0),
                sharpen_radius: f(p, "sharpenRadius", 2.0).clamp(1.0, 20.0),
                noise: f(p, "noise", 0.0).clamp(-100.0, 100.0),
                beam_pitch: i(p, "beamPitch", 6).clamp(2, 64) as u32,
                beam_amplitude: f(p, "beamAmplitude", 1.5).clamp(0.0, 8.0),
                highlights: colour(p, "highlights", [1.0, 1.0, 1.0, 1.0]),
                midtones: colour(p, "midtones", [176.0 / 255.0, 176.0 / 255.0, 176.0 / 255.0, 1.0]),
                shadows: colour(p, "shadows", [90.0 / 255.0, 90.0 / 255.0, 90.0 / 255.0, 1.0]),
                background: colour(p, "background", [0.0, 0.0, 0.0, 1.0]),
            },
        },
        _ => return None,
    })
}

macro_rules! cmd {
    ($id:literal, $label:literal, $menu:expr, $params:literal) => {
        CommandSpec {
            id: $id,
            label: $label,
            menu: $menu,
            shortcut: None,
            params: $params,
            enabled: crate::filters::has_filterable_layer,
            run: |s, p| crate::filters::run_filter(s, $id, p),
            journal: true,
        }
    };
}

/// The command specs.
pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!("filter.darkroom.sepia", "Sepia…", DARKROOM, r##"{"amount":0..100=80}"##),
        cmd!("filter.darkroom.chromaDenoise", "Chroma Denoise…", DARKROOM, r##"{"strength":1..100=40}"##),
        cmd!("filter.darkroom.filmNegative", "Film Negative → Positive…", DARKROOM, r##"{"mode":"auto|color|mono"="auto","clip":0..5=0.5}"##),
        cmd!(
            "filter.glitch.pixelSort",
            "Pixel Sort…",
            GLITCH,
            r##"{"low":0..255=60,"high":0..255=200,"direction":"rows|columns"="rows","order":"darkFirst|brightFirst"="darkFirst"}"##
        ),
        cmd!(
            "filter.glitch.datamosh",
            "Datamosh + Chromatic Aberration…",
            GLITCH,
            r##"{"block":4..64=16,"amount":0..100=35,"drift":0..64=12,"aberration":0..20=3,"seed":u32=7}"##
        ),
        cmd!("filter.glitch.retroConsole", "Retro Console (8-Bit)…", GLITCH, r##"{"size":1..64=6,"levels":2..8=4,"dither":bool=true}"##),
        cmd!(
            "filter.glitch.beamDither",
            "Beam Dither…",
            GLITCH,
            r##"{"algorithm":"beam|floydSteinberg|atkinson|jarvis|stucki|sierra|burkes|bayer2|bayer4|bayer8|threshold"="beam","mode":"mono|tonal|color"="mono","scale":1..32=3,"levels":2..8=2,"brightness":-100..100=0,"contrast":-100..100=0,"blur":0..20=0,"sharpen":0..300=0,"sharpenRadius":1..20=2,"noise":-100..100=0,"beamPitch":2..64=6,"beamAmplitude":0..8=1.5,"highlights":color=#FFFFFF,"midtones":color=#B0B0B0,"shadows":color=#5A5A5A,"background":color=#000000}"##
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;
    use photocraft_geom::Rect;
    use serde_json::json;

    fn session() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 48, "height": 32})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        s.edit("pattern", |doc, active| {
            let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
            for y in 0..32 {
                for x in 0..48 {
                    let v = ((x * 5 + y * 3) % 17) as f32 / 16.0;
                    surf.write_pixel(x, y, &[v, 1.0 - v, (x % 3) as f32 / 2.0, 1.0]);
                }
            }
            Ok(())
        })
        .unwrap();
        s
    }

    fn pixels(s: &Session) -> Vec<f32> {
        let d = s.active().unwrap();
        d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().read_region(Rect::new(0, 0, 48, 32))
    }

    #[test]
    fn ids_are_registered_once_with_a_menu_and_a_params_mapping() {
        let all = crate::command_specs();
        for id in IDS {
            let spec = crate::commands::find(id).unwrap_or_else(|| panic!("{id} not registered"));
            assert_eq!(all.iter().filter(|c| c.id == id).count(), 1, "{id} registered twice");
            assert_eq!(spec.menu.first(), Some(&"Filter"), "{id}");
            assert!(spec.params.starts_with('{'), "{id}");
            assert!(params_for(id, &Value::Null).is_some(), "{id} has no params mapping");
            // The shared chain (filters → filters_ext → here) resolves it too, so Last Filter,
            // smart filters and the dialog preview all find it.
            assert!(crate::filters::params_for(id, &Value::Null).is_some(), "{id} not reachable from filters::params_for");
        }
        assert!(params_for("filter.glitch.nope", &Value::Null).is_none());
    }

    #[test]
    fn every_port_runs_changes_pixels_and_undoes_in_one_step() {
        for id in IDS {
            let mut s = session();
            let before = pixels(&s);
            let depth = s.active().unwrap().history.past_len();
            let r = s.execute(id, json!({})).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(r.get("filter").is_some(), "{id}");
            assert_ne!(pixels(&s), before, "{id} changed nothing");
            assert_eq!(s.active().unwrap().history.past_len(), depth + 1, "{id} is one history step");
            s.execute("edit.undo", json!({})).unwrap();
            assert_eq!(pixels(&s), before, "{id} undo");
        }
    }

    #[test]
    fn ports_work_at_16_and_32_bit_and_in_other_colour_models() {
        for (depth, mode) in [(16, "rgb"), (32, "rgb"), (8, "cmyk"), (16, "lab"), (8, "gray")] {
            let mut s = Session::new();
            s.execute("file.new", json!({"width": 24, "height": 16, "depth": depth, "mode": mode})).unwrap();
            s.execute("layer.new.layer", json!({})).unwrap();
            s.execute("edit.fill", json!({"color": "#3366cc"})).ok();
            for id in IDS {
                s.execute(id, json!({})).unwrap_or_else(|e| panic!("{id} at {depth}-bit {mode}: {e}"));
            }
        }
    }

    #[test]
    fn disabled_without_pixel_layer() {
        let mut s = Session::new();
        for id in IDS {
            assert!(!s.is_enabled(id), "{id}");
        }
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        s.execute("layer.newAdjustmentLayer.invert", json!({})).unwrap();
        for id in IDS {
            assert!(!s.is_enabled(id), "{id} enabled on an adjustment layer");
        }
    }

    #[test]
    fn hostile_params_are_clamped_never_panic() {
        let hostile = json!({
            "amount": 1e300, "strength": -7, "mode": 42, "clip": 1e9,
            "low": 9e18, "high": -5, "direction": [1, 2], "order": null,
            "block": 0, "drift": -1, "aberration": "x", "seed": 1e30,
            "size": -3, "levels": 99, "dither": "yes",
            "algorithm": "", "scale": 0, "brightness": 1e9, "blur": 1e9, "sharpen": -1, "sharpenRadius": 0, "noise": -1e9,
            "beamPitch": 0, "beamAmplitude": 1e9, "highlights": "#zzzzzz", "midtones": [2, -1], "shadows": "#1", "background": 7
        });
        for id in IDS {
            let mut s = session();
            let before = pixels(&s);
            // Either a result or an error; the point is no panic and a document that undoes.
            if s.execute(id, hostile.clone()).is_ok() {
                s.execute("edit.undo", json!({})).unwrap();
            }
            assert_eq!(pixels(&s), before, "{id}");
        }
    }

    #[test]
    fn params_map_to_algorithm_units() {
        assert_eq!(
            params_for("filter.glitch.pixelSort", &json!({"low": 200, "high": 40, "direction": "columns", "order": "brightFirst"})),
            Some(FilterParams::PixelSort { low: 200.0, high: 40.0, vertical: true, reverse: true })
        );
        assert_eq!(
            params_for("filter.glitch.datamosh", &json!({"block": 999, "amount": 35, "drift": 12, "seed": 3})),
            Some(FilterParams::Datamosh { block: 64, amount: 0.35, drift: 12, aberration: 3.0, seed: 3 })
        );
        assert_eq!(
            params_for("filter.darkroom.filmNegative", &json!({"mode": "mono", "clip": 2})),
            Some(FilterParams::FilmNegative { mode: NegativeMode::Mono, clip: 2.0 })
        );
        let Some(FilterParams::BeamDither { spec }) =
            params_for("filter.glitch.beamDither", &json!({"algorithm": "atkinson", "mode": "tonal", "shadows": "#5A5A5A", "highlights": [1, 0.5, 0]}))
        else {
            panic!("not a beam dither")
        };
        assert_eq!((spec.algorithm, spec.mode), (DitherAlgorithm::Atkinson, BeamMode::Tonal));
        assert!((spec.shadows[0] - 90.0 / 255.0).abs() < 1e-6 && spec.shadows[0] == spec.shadows[1]);
        assert_eq!(spec.highlights, [1.0, 0.5, 0.0, 1.0]);
    }

    #[test]
    fn selection_limits_the_ports() {
        let mut s = session();
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 12, "height": 12})).unwrap();
        let before = pixels(&s);
        for id in ["filter.glitch.pixelSort", "filter.darkroom.filmNegative", "filter.glitch.beamDither"] {
            s.execute(id, json!({"low": 0, "high": 255})).unwrap();
            let after = pixels(&s);
            let far = (28 * 48 + 40) * 4;
            assert_eq!(after[far..far + 4], before[far..far + 4], "{id} leaked outside the selection");
            assert_ne!(after[..48], before[..48], "{id} did nothing inside the selection");
            s.execute("edit.undo", json!({})).unwrap();
        }
    }

    #[test]
    fn last_filter_repeats_a_port() {
        let mut s = session();
        s.execute("filter.darkroom.sepia", json!({"amount": 50})).unwrap();
        let once = pixels(&s);
        s.execute("filter.lastFilter", json!({})).unwrap();
        assert_ne!(pixels(&s), once);
        assert_eq!(s.active().unwrap().history.undo_label(), Some("Sepia"));
    }
}
