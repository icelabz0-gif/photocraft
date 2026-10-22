use std::sync::Arc;

use serde_json::{Value, json};

use crate::Session;
use photocraft_color::BlendMode;
use photocraft_doc::{Adjustment, Fill, Layer, LayerContent, LayerId, LayerMask};

fn session(depth: u32) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48, "depth": depth})).unwrap();
    s
}

fn active(s: &Session) -> LayerId {
    s.active().unwrap().active_layer.unwrap()
}

fn layer_px(s: &Session, x: i32, y: i32) -> Vec<f32> {
    let d = s.active().unwrap();
    d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().pixel(x, y)
}

// ------------------------------------------------------------------ styles

#[test]
fn style_preset_new_from_an_explicit_effect_list() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 16, "height": 16})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    // The Layer Style dialog's "New Style…" saves its pending state, not the layer.
    let r = s
        .execute(
            "style.presets.new",
            json!({"name": "Pending", "effects": [["stroke", {"size": 6, "color": "#123456"}]], "blend": "Multiply", "fillOpacity": 40}),
        )
        .unwrap();
    assert_eq!(r["name"], json!("Pending"));
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("style.presets.apply", json!({"preset": "Pending"})).unwrap();
    let d = s.active().unwrap();
    let l = d.doc.layer(d.active_layer.unwrap()).unwrap();
    assert_eq!(l.effects.items.len(), 1);
    assert!(matches!(&l.effects.items[0], photocraft_doc::Effect::Stroke(st) if st.size == 6.0));
    assert_eq!(l.blend, photocraft_color::BlendMode::Multiply);
    assert!((l.fill_opacity - 0.4).abs() < 1e-6);
    // Malformed lists are graceful errors.
    assert!(s.execute("style.presets.new", json!({"effects": [["nope", {}]]})).is_err());
    assert!(s.execute("style.presets.new", json!({"effects": ["stroke"]})).is_err());
}

// ------------------------------------------------------------------ gradients

#[test]
fn gradient_builtins_and_listing() {
    let mut s = Session::new();
    let v = s.execute("gradient.presets.list", json!({})).unwrap();
    let groups = v["groups"].as_array().unwrap();
    let names: Vec<&str> = groups.iter().map(|g| g["name"].as_str().unwrap()).collect();
    assert_eq!(names[0], "Basics");
    assert!(names.contains(&"Blues") && names.contains(&"Grays"));
    assert_eq!(v["current"]["name"], "Foreground to Background");
    assert_eq!(groups[0]["presets"][1]["transparency"][1], json!([1.0, 0.0]));
}

#[test]
fn rainbow_gradient_presets() {
    let mut s = session(8);
    let v = s.execute("gradient.presets.list", json!({})).unwrap();
    let group = v["groups"].as_array().unwrap().iter().find(|g| g["name"] == "Rainbows").unwrap();
    let presets = group["presets"].as_array().unwrap();
    let names: Vec<&str> = presets.iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Rainbow", "Pastel Rainbow", "Neon Rainbow", "Oil Slick", "Rainbow Fade"]);
    // The three hue ramps share the same 13 evenly spaced hues: smooth, not the 7 coarse ROYGBIV points.
    for p in [&presets[0], &presets[1], &presets[2]] {
        let stops = p["stops"].as_array().unwrap();
        assert_eq!(stops.len(), 13, "{}", p["name"]);
        assert_eq!(stops[0][0], json!(0.0));
        assert_eq!(stops[12][0], json!(1.0));
    }
    assert_eq!(presets[3]["stops"].as_array().unwrap().len(), 11, "oil slick");
    assert_eq!(presets[4]["transparency"], json!([[0.0, 0.0], [0.125, 100.0], [0.875, 100.0], [1.0, 0.0]]));
    // Rainbow Fade paints opaque across the middle and transparent at both ends.
    s.execute("gradient.presets.select", json!({"preset": "Rainbow Fade"})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("paint.gradient", json!({"from": [0, 0], "to": [63, 0]})).unwrap();
    assert!(layer_px(&s, 0, 5)[3] < 0.1, "left edge transparent: {:?}", layer_px(&s, 0, 5));
    assert!(layer_px(&s, 63, 5)[3] < 0.1, "right edge transparent: {:?}", layer_px(&s, 63, 5));
    assert!(layer_px(&s, 32, 5)[3] > 0.95, "middle opaque: {:?}", layer_px(&s, 32, 5));
}

#[test]
fn gradient_tool_uses_selected_preset_at_every_depth() {
    for depth in [8, 16, 32] {
        let mut s = session(depth);
        // Default (Foreground to Background): black → white, as before presets existed.
        s.execute("paint.gradient", json!({"from": [0, 0], "to": [63, 0]})).unwrap();
        assert!(layer_px(&s, 0, 10)[0] < 0.02, "depth {depth}");
        assert!(layer_px(&s, 63, 10)[0] > 0.98);
        s.execute("gradient.presets.select", json!({"preset": "Red 01"})).unwrap();
        s.execute("paint.gradient", json!({"from": [0, 0], "to": [63, 0]})).unwrap();
        let left = layer_px(&s, 0, 10);
        // Red 01 starts at #7f0000.
        assert!((left[0] - 127.0 / 255.0).abs() < 0.02 && left[1] < 0.01, "depth {depth}: {left:?}");
        // Explicit stops and a named preset override the selection.
        s.execute("paint.gradient", json!({"from": [0, 0], "to": [63, 0], "stops": [[0, "#00ff00"], [1, "#0000ff"]]})).unwrap();
        assert!(layer_px(&s, 0, 10)[1] > 0.98);
        s.execute("paint.gradient", json!({"from": [0, 0], "to": [63, 0], "gradient": "Black, White"})).unwrap();
        assert!(layer_px(&s, 63, 10)[0] > 0.98);
    }
}

#[test]
fn foreground_to_transparent_paints_alpha() {
    let mut s = session(8);
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("gradient.presets.select", json!({"preset": "Foreground to Transparent"})).unwrap();
    s.execute("paint.gradient", json!({"from": [0, 0], "to": [63, 0]})).unwrap();
    assert!(layer_px(&s, 0, 5)[3] > 0.95);
    assert!(layer_px(&s, 63, 5)[3] < 0.05);
    let mid = layer_px(&s, 32, 5)[3];
    assert!((mid - 0.5).abs() < 0.05, "{mid}");
}

/// `gradient.presets.stop` edits the Gradient tool's current gradient in place (the stops the
/// Gradient Editor window shows): a new stop takes the colour under it, a "foreground" stop stays
/// live, opacity stops materialize on first edit, and the tool paints with the edited gradient.
#[test]
fn preset_stop_edits_the_current_gradient() {
    let mut s = session(8);
    // A new stop mid-way samples the ramp there (black → white: mid gray).
    let v = s.execute("gradient.presets.stop", json!({"action": "add", "location": 0.5})).unwrap();
    let stops = v["current"]["stops"].as_array().unwrap();
    assert_eq!(stops.len(), 3, "{v}");
    assert_eq!(stops[1], json!([0.5, "#808080"]), "sorted by location, the ramp colour at 0.5");
    // Recolour it and repaint: the tool gradient changed (mid of the stroke is the new colour).
    s.execute("gradient.presets.stop", json!({"action": "color", "index": 1, "color": "#ff0000"})).unwrap();
    s.execute("paint.gradient", json!({"from": [0, 0], "to": [63, 0]})).unwrap();
    let px = layer_px(&s, 32, 10);
    assert!(px[0] > 0.9 && px[1] < 0.1, "{px:?}");
    // A "foreground" stop stays live: repaint after changing the foreground picks it up.
    s.execute("gradient.presets.stop", json!({"action": "color", "index": 0, "color": "foreground"})).unwrap();
    s.execute("tools.setColors", json!({"foreground": "#0000ff"})).unwrap();
    s.execute("paint.gradient", json!({"from": [0, 0], "to": [63, 0]})).unwrap();
    let px = layer_px(&s, 0, 10);
    assert!(px[2] > 0.9 && px[0] < 0.1, "{px:?}");
    // Opacity stops materialize the two opaque ends on first edit.
    let v = s.execute("gradient.presets.stop", json!({"action": "add", "kind": "opacity", "location": 0.5, "opacity": 40})).unwrap();
    assert_eq!(v["current"]["transparency"], json!([[0.0, 100.0], [0.5, 40.0], [1.0, 100.0]]), "{v}");
    s.execute("paint.gradient", json!({"from": [0, 0], "to": [63, 0]})).unwrap();
    // Paint on a fresh layer: the background already holds opaque paint to composite onto.
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("paint.gradient", json!({"from": [0, 0], "to": [63, 0]})).unwrap();
    let mid = layer_px(&s, 32, 10)[3];
    assert!((mid - 0.4).abs() < 0.05, "{mid}");
    // Guard rails: in-range indexes, known actions, two stops minimum, no midpoints.
    assert!(s.execute("gradient.presets.stop", json!({"action": "move", "index": 99, "location": 0.5})).is_err());
    assert!(s.execute("gradient.presets.stop", json!({"action": "explode"})).is_err());
    assert!(s.execute("gradient.presets.stop", json!({"action": "delete", "index": 0})).is_ok());
    assert!(s.execute("gradient.presets.stop", json!({"action": "delete", "index": 0})).is_err(), "a gradient keeps at least 2 colour stops");
    assert!(s.execute("gradient.presets.stop", json!({"action": "midpoint", "index": 0, "location": 0.4})).is_err());
}

/// A many-stop gradient survives editor gymnastics: five adds build a seven-stop ramp, a stop
/// dragged past its neighbour keeps the list sorted, the paint follows the edited stops, and
/// trimming back down ends at a plain two-stop gradient.
#[test]
fn preset_stop_survives_a_complex_many_stop_ramp() {
    let mut s = session(8);
    s.execute("gradient.presets.select", json!({"stops": [[0.0, "#000000"], [1.0, "#ffffff"]]})).unwrap();
    for (i, c) in ["#ff0000", "#00ff00", "#0000ff", "#ffff00", "#ff00ff"].iter().enumerate() {
        let t = (i + 1) as f64 / 6.0;
        s.execute("gradient.presets.stop", json!({"action": "add", "location": t, "color": c})).unwrap();
    }
    let v = s.execute("gradient.presets.list", json!({})).unwrap();
    assert_eq!(v["current"]["stops"].as_array().unwrap().len(), 7, "{v}");
    // Drag the yellow stop (index 4, at 4/6) past the magenta one (at 5/6).
    s.execute("gradient.presets.stop", json!({"action": "move", "index": 4, "location": 0.95})).unwrap();
    let stops = s.execute("gradient.presets.list", json!({})).unwrap()["current"]["stops"].as_array().unwrap().clone();
    let locs: Vec<f64> = stops.iter().map(|s| s[0].as_f64().unwrap()).collect();
    let mut in_order = locs.clone();
    in_order.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(locs, in_order, "stops stay sorted after a crossing move: {stops:?}");
    assert_eq!(stops[6], json!([1.0, "#ffffff"]));
    // The paint follows: near the moved stop the stroke is the stop's yellow.
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("paint.gradient", json!({"from": [0, 0], "to": [63, 0]})).unwrap();
    let px = layer_px(&s, 60, 10);
    assert!(px[0] > 0.95 && px[1] > 0.95 && px[2] < 0.3, "{px:?}");
    // Trim everything back down to the two end stops.
    for _ in 0..5 {
        s.execute("gradient.presets.stop", json!({"action": "delete", "index": 1})).unwrap();
    }
    let stops = s.execute("gradient.presets.list", json!({})).unwrap()["current"]["stops"].as_array().unwrap().clone();
    assert_eq!(stops, json!([[0.0, "#000000"], [1.0, "#ffffff"]]).as_array().unwrap().clone(), "{stops:?}");
}

/// An opacity hole paints where it is put, follows a move, and once the gradient is fully
/// opaque again the stored opacity stops normalize away (no pinned 100 % ends).
#[test]
fn preset_stop_opacity_hole_moves_and_normalizes() {
    let mut s = session(8);
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("gradient.presets.stop", json!({"action": "add", "kind": "opacity", "location": 0.5, "opacity": 0})).unwrap();
    s.execute("paint.gradient", json!({"from": [0, 0], "to": [63, 0]})).unwrap();
    assert!(layer_px(&s, 0, 5)[3] > 0.95);
    assert!(layer_px(&s, 32, 5)[3] < 0.05);
    assert!(layer_px(&s, 63, 5)[3] > 0.95);
    // The hole follows the moved stop (index 1 is the 0 % stop between the opaque ends).
    s.execute("gradient.presets.stop", json!({"action": "move", "kind": "opacity", "index": 1, "location": 0.25})).unwrap();
    // Paint on fresh layers: painting over earlier strokes would composite their alpha.
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("paint.gradient", json!({"from": [0, 0], "to": [63, 0]})).unwrap();
    assert!(layer_px(&s, 20, 5)[3] < 0.15, "{:?}", layer_px(&s, 20, 5));
    let far = layer_px(&s, 48, 5)[3];
    assert!(far > 0.55 && far < 0.85, "{far}");
    // Deleting it returns the gradient to fully opaque, stored as no opacity stops at all.
    s.execute("gradient.presets.stop", json!({"action": "delete", "kind": "opacity", "index": 1})).unwrap();
    let v = s.execute("gradient.presets.list", json!({})).unwrap();
    assert_eq!(v["current"]["transparency"], json!([]), "{v}");
}

/// The edited current gradient saves as a preset, and re-selecting it paints the same pixels.
#[test]
fn edited_preset_saves_and_roundtrips() {
    let mut s = session(8);
    s.execute("gradient.presets.stop", json!({"action": "add", "location": 0.5, "color": "#ff0000"})).unwrap();
    s.execute("gradient.presets.new", json!({"name": "My Gradient"})).unwrap();
    s.execute("gradient.presets.select", json!({"preset": "Red 01"})).unwrap();
    s.execute("gradient.presets.select", json!({"preset": "My Gradient"})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("paint.gradient", json!({"from": [0, 0], "to": [63, 0]})).unwrap();
    let px = layer_px(&s, 32, 10);
    assert!(px[0] > 0.9 && px[1] < 0.1, "{px:?}");
}

/// Issue #465: explicit `transparency` stops apply with `colors`, a `gradient` preset, explicit
/// `stops` and the current gradient alike.
#[test]
fn paint_gradient_honours_explicit_transparency_stops() {
    let transparency = json!([[0, 65], [1, 0]]);
    for depth in [8, 16, 32] {
        for extra in [
            json!({"colors": ["#07111d", "#07111d"]}),
            json!({"gradient": "Black, White"}),
            json!({"gradient": "Foreground to Transparent", "transparency": [[0, 65], [0.5, 65], [1, 0]]}),
            json!({"stops": [[0, "#07111d"], [1, "#07111d"]]}),
            json!({}),
        ] {
            let mut s = Session::new();
            s.execute("file.new", json!({"width": 16, "height": 16, "depth": depth, "background": "transparent"})).unwrap();
            let mut p = json!({"from": [0, 0], "to": [0, 15], "transparency": transparency});
            for (k, v) in extra.as_object().unwrap() {
                p[k] = v.clone();
            }
            s.execute("paint.gradient", p).unwrap();
            let (top, bottom) = (layer_px(&s, 8, 0)[3], layer_px(&s, 8, 15)[3]);
            assert!((top - 0.65).abs() < 0.03, "depth {depth} {extra}: top alpha {top}");
            assert!(bottom < 0.03, "depth {depth} {extra}: bottom alpha {bottom}");
        }
    }
    // `colors` keeps its colour; a malformed stop list is an error, not a silent opaque fill.
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 16, "height": 16, "background": "transparent"})).unwrap();
    s.execute("paint.gradient", json!({"from": [0, 0], "to": [0, 15], "colors": ["#07111d", "#07111d"], "transparency": transparency})).unwrap();
    let top = layer_px(&s, 8, 0);
    assert!((top[2] - 29.0 / 255.0).abs() < 0.01, "{top:?}");
    for bad in [json!("65"), json!([[0]]), json!([[0, "a"]])] {
        assert!(s.execute("paint.gradient", json!({"from": [0, 0], "to": [0, 15], "colors": ["#000000"], "transparency": bad})).is_err());
    }
}

#[test]
fn gradient_apply_creates_fill_layer_and_select_recolours_it() {
    let mut s = session(8);
    let r = s.execute("gradient.presets.apply", json!({"preset": "Blue 01", "angle": 0})).unwrap();
    let id = LayerId(r["layer"].as_u64().unwrap());
    let stops = |s: &Session| match &s.active().unwrap().doc.layer(id).unwrap().content {
        LayerContent::Fill(Fill::Gradient { stops, angle, .. }) => (stops.clone(), *angle),
        _ => panic!("not a gradient fill"),
    };
    let (st, angle) = stops(&s);
    assert_eq!(angle, 0.0);
    assert!((st[0].1.c[2] - 0x91 as f32 / 255.0).abs() < 1e-3);
    // Clicking another preset with the fill layer selected changes it (one undo step).
    let r = s.execute("gradient.presets.select", json!({"preset": "Green 01"})).unwrap();
    assert_eq!(r["layer"], json!(id.0));
    assert!(stops(&s).0[0].1.c[1] > 0.25);
    s.execute("edit.undo", json!({})).unwrap();
    assert!((stops(&s).0[0].1.c[2] - 0x91 as f32 / 255.0).abs() < 1e-3);
}

#[test]
fn gradient_new_rename_move_delete_groups() {
    let mut s = Session::new();
    let rev = s.prefs.rev();
    s.execute("gradient.presets.select", json!({"stops": [[0, "#112233"], [0.5, "foreground"], [1, "#ffffff"]], "transparency": [[0, 100], [1, 50]]})).unwrap();
    let r = s.execute("gradient.presets.new", json!({"name": "Mine", "group": "My Gradients"})).unwrap();
    assert_eq!(r["group"], "My Gradients");
    assert!(s.prefs.rev() > rev, "preset edits mark preferences dirty");
    let dup = s.execute("gradient.presets.new", json!({"name": "Mine"})).unwrap();
    assert_eq!(dup["name"], "Mine 2");
    s.execute("gradient.presets.edit", json!({"action": "rename", "preset": "Mine", "name": "Ours"})).unwrap();
    assert!(s.execute("gradient.presets.edit", json!({"action": "rename", "preset": "Ours", "name": "Mine 2"})).is_err());
    s.execute("gradient.presets.edit", json!({"action": "move", "preset": "Mine 2", "to": "My Gradients", "index": 0})).unwrap();
    let v = s.execute("gradient.presets.list", json!({})).unwrap();
    let mine = v["groups"].as_array().unwrap().iter().find(|g| g["name"] == "My Gradients").unwrap().clone();
    assert_eq!(mine["presets"][0]["name"], "Mine 2");
    assert_eq!(mine["presets"][1]["stops"][1], json!([0.5, "foreground"]));
    assert_eq!(mine["presets"][1]["transparency"][1], json!([1.0, 50.0]));
    s.execute("gradient.presets.edit", json!({"action": "delete", "preset": ["Ours", "Mine 2"]})).unwrap();
    s.execute("gradient.presets.edit", json!({"action": "deleteGroup", "group": "My Gradients"})).unwrap();
    s.execute("gradient.presets.edit", json!({"action": "deleteGroup", "group": "Blues"})).unwrap();
    s.execute("gradient.presets.reset", json!({})).unwrap();
    let v = s.execute("gradient.presets.list", json!({})).unwrap();
    assert!(v["groups"].as_array().unwrap().iter().any(|g| g["name"] == "Blues"));
    // Bad params.
    assert!(s.execute("gradient.presets.select", json!({"preset": "nope"})).is_err());
    assert!(s.execute("gradient.presets.select", json!({"stops": [[0, "#zzzzzz"], [1, "#000000"]]})).is_err());
    assert!(s.execute("gradient.presets.edit", json!({"action": "explode"})).is_err());
}

#[test]
fn gradient_rename_group_refuses_a_name_another_group_has() {
    let mut s = Session::new();
    s.execute("gradient.presets.edit", json!({"action": "newGroup", "name": "Pastels"})).unwrap();
    let names = |s: &mut Session| -> Vec<Value> {
        s.execute("gradient.presets.list", json!({})).unwrap()["groups"].as_array().unwrap().iter().map(|g| g["name"].clone()).collect()
    };
    let before = names(&mut s);
    assert!(before.contains(&json!("Blues")));
    let err = s.execute("gradient.presets.edit", json!({"action": "renameGroup", "group": "Pastels", "name": "Blues"})).unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err}");
    assert_eq!(names(&mut s), before, "a refused rename leaves the groups unchanged");
    // Renaming a group to its own name is fine.
    s.execute("gradient.presets.edit", json!({"action": "renameGroup", "group": "Pastels", "name": "Pastels"})).unwrap();
    assert_eq!(names(&mut s), before);
}

#[test]
fn presets_persist_through_preferences_json() {
    let mut s = Session::new();
    s.execute("gradient.presets.new", json!({"name": "Saved", "stops": [[0, "#ff0000"], [1, "#0000ff"]]})).unwrap();
    s.execute("tool.presets.new", json!({"name": "Big Brush", "tool": "brush", "options": {"brush": {"size": 300.0}}})).unwrap();
    s.execute("shape.presets.new", json!({"name": "Tri", "path": "M 0 0 L 10 0 L 5 10 Z"})).unwrap();
    s.execute("style.presets.edit", json!({"action": "deleteGroup", "group": "Buttons"})).unwrap();
    let text = s.prefs_to_json();
    let mut t = Session::new();
    t.load_prefs_json(&text).unwrap();
    let names = |t: &mut Session, cmd: &str, key: &str| -> Vec<String> {
        let v = t.execute(cmd, json!({})).unwrap();
        v["groups"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g[key].as_array().unwrap().iter().map(|i| i["name"].as_str().unwrap().to_string()).collect::<Vec<_>>())
            .collect()
    };
    assert!(names(&mut t, "gradient.presets.list", "presets").contains(&"Saved".to_string()));
    assert!(names(&mut t, "shape.presets.list", "shapes").contains(&"Tri".to_string()));
    let styles = t.execute("style.presets.list", json!({})).unwrap();
    assert!(!styles["groups"].as_array().unwrap().iter().any(|g| g["name"] == "Buttons"));
    let tp = t.execute("tool.presets.list", json!({"tool": "Brush Tool"})).unwrap();
    assert!(tp["presets"].as_array().unwrap().iter().any(|p| p["name"] == "Big Brush"));
    // Older preference files without presets keep the defaults.
    let mut u = Session::new();
    u.load_prefs_json("{}").unwrap();
    assert_eq!(names(&mut u, "gradient.presets.list", "presets").len(), Session::new().presets.gradients.iter().map(|g| g.items.len()).sum::<usize>());
}

// ------------------------------------------------------------------ patterns

#[test]
fn pattern_groups_select_apply_and_sync() {
    let mut s = session(8);
    let v = s.execute("pattern.presets.list", json!({})).unwrap();
    let groups = v["groups"].as_array().unwrap();
    assert_eq!(groups[0]["name"], "Geometric");
    assert_eq!(groups[0]["patterns"].as_array().unwrap().len(), 4);
    s.execute("pattern.presets.select", json!({"pattern": "Bricks"})).unwrap();
    // Pattern fill layers and Fill default to the selected pattern.
    let r = s.execute("pattern.presets.apply", json!({})).unwrap();
    let id = LayerId(r["layer"].as_u64().unwrap());
    let name_of = |s: &Session| match &s.active().unwrap().doc.layer(id).unwrap().content {
        LayerContent::Fill(Fill::Pattern { name, .. }) => name.clone(),
        _ => panic!(),
    };
    assert_eq!(name_of(&s), "Bricks");
    let r = s.execute("pattern.presets.select", json!({"pattern": "Dots"})).unwrap();
    assert_eq!(r["layer"], json!(id.0));
    assert_eq!(name_of(&s), "Dots");
    // A newly defined pattern lands in the trailing group, and can be moved / grouped.
    let r = s.execute("pattern.presets.new", json!({"name": "Mine", "rect": [0, 0, 8, 8], "group": "Custom"})).unwrap();
    let pid = r["pattern"].as_str().unwrap().to_string();
    let v = s.execute("pattern.presets.list", json!({})).unwrap();
    let custom = v["groups"].as_array().unwrap().iter().find(|g| g["name"] == "Custom").unwrap().clone();
    assert_eq!(custom["patterns"][0]["id"], json!(pid));
    s.execute("pattern.presets.edit", json!({"action": "move", "preset": "Mine", "to": "Geometric"})).unwrap();
    s.execute("pattern.presets.edit", json!({"action": "rename", "preset": "Mine", "name": "Ours"})).unwrap();
    s.execute("pattern.presets.edit", json!({"action": "delete", "preset": "Ours"})).unwrap();
    let v = s.execute("pattern.presets.list", json!({})).unwrap();
    assert_eq!(v["groups"][0]["patterns"].as_array().unwrap().len(), 4);
    // Deleting a group deletes its patterns from the library.
    s.execute("pattern.presets.edit", json!({"action": "deleteGroup", "group": "Textures"})).unwrap();
    assert!(crate::pattern_cmds::resolve(&s, "Weave").is_none());
}

#[test]
fn pattern_batch_delete_keeps_the_library_when_one_name_is_missing() {
    let mut s = session(8);
    let before = s.execute("pattern.presets.list", json!({})).unwrap();
    let err = s.execute("pattern.presets.edit", json!({"action": "delete", "preset": ["Bricks", "missing"]})).unwrap_err();
    assert!(err.to_string().contains("missing"), "{err}");
    assert_eq!(s.execute("pattern.presets.list", json!({})).unwrap(), before);
    assert!(crate::pattern_cmds::resolve(&s, "Bricks").is_some());
}

#[test]
fn pattern_batch_delete_rejects_non_string_members_without_deleting() {
    for invalid in [json!(null), json!(7), json!({})] {
        let mut s = session(8);
        let before = s.execute("pattern.presets.list", json!({})).unwrap();
        assert!(s.execute("pattern.presets.edit", json!({"action": "delete", "preset": ["Bricks", invalid]})).is_err());
        assert_eq!(s.execute("pattern.presets.list", json!({})).unwrap(), before);
    }
}

#[test]
fn pattern_batch_delete_refuses_a_document_only_pattern_up_front() {
    let mut s = session(8);
    let added = s.execute("edit.definePattern", json!({"name": "Document Only"})).unwrap();
    let id = added["pattern"].as_str().unwrap().to_string();
    let pattern = s.patterns.items.iter().find(|p| p.id == id).unwrap().clone();
    s.edit("Embed pattern fixture", |doc, _| {
        doc.patterns.push(pattern.clone());
        Ok(())
    })
    .unwrap();
    s.execute("pattern.delete", json!({"pattern": id})).unwrap();
    let before = s.patterns.items.clone();
    assert!(s.execute("pattern.presets.edit", json!({"action": "delete", "preset": ["Bricks", id]})).is_err());
    assert_eq!(s.patterns.items, before, "nothing is deleted when one entry is not in the library");
}

// ------------------------------------------------------------------ styles

#[test]
fn styles_apply_to_selected_layers_and_new_from_layer() {
    let mut s = session(16);
    let a = s.execute("layer.new.layer", json!({})).unwrap()["layer"].as_u64().map(LayerId).unwrap_or_else(|| active(&s));
    let b = s.execute("layer.new.layer", json!({})).unwrap()["layer"].as_u64().map(LayerId).unwrap_or_else(|| active(&s));
    s.execute("layer.select", json!({"layer": a.0})).unwrap();
    s.execute("layer.select", json!({"layer": b.0, "mode": "add"})).unwrap();
    let r = s.execute("style.presets.apply", json!({"preset": "Gold"})).unwrap();
    assert_eq!(r["layers"].as_array().unwrap().len(), 2);
    let fx = |s: &Session, id: LayerId| s.active().unwrap().doc.layer(id).unwrap().effects.items.len();
    assert_eq!((fx(&s, a), fx(&s, b)), (3, 3));
    // ⇧-click adds; plain apply replaces; Default Style clears.
    s.execute("style.presets.apply", json!({"preset": "Black Stroke", "add": true, "layer": a.0})).unwrap();
    assert_eq!(fx(&s, a), 4);
    s.execute("style.presets.apply", json!({"preset": "Hollow", "layer": b.0})).unwrap();
    let lb = s.active().unwrap().doc.layer(b).unwrap().clone();
    assert_eq!((lb.effects.items.len(), lb.fill_opacity), (1, 0.0));
    s.execute("style.presets.apply", json!({"preset": "Default Style (None)", "layer": b.0})).unwrap();
    assert_eq!(s.active().unwrap().doc.layer(b).unwrap().fill_opacity, 1.0);
    // One undo step per apply.
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(fx(&s, b), 1);
    // New Style from the layer, applied elsewhere.
    s.execute("layer.select", json!({"layer": a.0})).unwrap();
    s.execute("style.presets.new", json!({"name": "From A", "group": "Mine"})).unwrap();
    s.execute("style.presets.apply", json!({"preset": "From A", "layer": b.0})).unwrap();
    assert_eq!(fx(&s, b), 4);
    assert!(s.execute("style.presets.apply", json!({"preset": "nope"})).is_err());
    // Thumbnails render the effect around the swatch.
    let st = s.presets.styles[0].items.iter().find(|x| x.name == "Outer Glow").unwrap().clone();
    let px = super::styles::thumbnail(&s, &st, 48);
    assert_eq!(px.len(), 48 * 48 * 4);
    assert!(px[(4 * 48 + 24) * 4 + 3] == 0 || px[(7 * 48 + 24) * 4 + 3] > 0, "glow reaches outside the square");
    assert!(px[(24 * 48 + 24) * 4 + 3] == 255);
}

#[test]
fn styles_with_pattern_overlay_copy_the_pattern() {
    let mut s = session(8);
    s.execute("layer.new.layer", json!({})).unwrap();
    let id = active(&s);
    s.presets.styles[0].items.push(super::styles::StylePreset {
        name: "Bricky".into(),
        effects: vec![crate::layer_style::effect_from_params("patternOverlay", &json!({"pattern": "Bricks"})).unwrap()],
        blend: None,
        fill_opacity: None,
    });
    s.execute("style.presets.apply", json!({"preset": "Bricky", "layer": id.0})).unwrap();
    assert!(s.active().unwrap().doc.patterns.iter().any(|p| p.name == "Bricks"));
}

// ------------------------------------------------------------------ shapes

#[test]
fn shape_path_language_parses() {
    let p = super::shapes::parse("M 0 0 L 10 0 L 10 10 Z ! O 5 5 2").unwrap();
    assert_eq!(p.subpaths.len(), 2);
    assert_eq!(p.subpaths[0].knots.len(), 3);
    assert_eq!(p.subpaths[1].op, photocraft_doc::vector::PathOp::Subtract);
    assert!(super::shapes::parse("M 0 0 L x").is_err());
    assert!(super::shapes::parse("").is_err());
    assert!(super::shapes::parse("L 1 1").is_err());
    // Every built-in parsed and has an area.
    for g in super::shapes::builtin() {
        assert!(!g.items.is_empty(), "{}", g.name);
        for sh in g.items {
            let cov = super::shapes::thumbnail(&sh.path, 32);
            assert!(cov.iter().sum::<f32>() > 20.0, "{} is empty", sh.name);
        }
    }
    let n: usize = super::shapes::builtin().iter().map(|g| g.items.len()).sum();
    assert!(n >= 30, "{n}");
}

#[test]
fn shapes_place_as_shape_layers() {
    let mut s = session(8);
    let r = s.execute("shape.presets.place", json!({"preset": "Heart", "rect": [10, 10, 40, 20], "fill": "#ff0000"})).unwrap();
    let id = LayerId(r["layer"].as_u64().unwrap());
    let l = s.active().unwrap().doc.layer(id).unwrap().clone();
    assert_eq!(l.name, "Heart");
    let LayerContent::Shape(sh) = &l.content else { panic!("not a shape layer") };
    let (x0, y0, x1, y1) = sh.path.control_bounds().unwrap();
    // Keep aspect: 20 px tall, centred horizontally in the 40 px rect.
    assert!((y0 - 10.0).abs() < 0.01 && (y1 - 30.0).abs() < 0.01, "{y0} {y1}");
    // The heart's box is 100 × 92: 20 px tall → 21.7 px wide, centred on x = 30.
    assert!(((x1 - x0) - 20.0 * 100.0 / 92.0).abs() < 0.05 && ((x0 + x1) / 2.0 - 30.0).abs() < 0.01, "{x0} {x1}");
    // Free (dragged without ⇧) fills the rect; negative drags flip.
    let r = s.execute("shape.presets.place", json!({"preset": "Arrow Right", "rect": [50, 40, -40, -20], "keepAspect": false})).unwrap();
    let l = s.active().unwrap().doc.layer(LayerId(r["layer"].as_u64().unwrap())).unwrap().clone();
    let LayerContent::Shape(sh) = &l.content else { panic!() };
    assert_eq!(sh.path.control_bounds().unwrap(), (10.0, 20.0, 50.0, 40.0));
    // Default placement: centred, half the short side.
    s.execute("shape.presets.place", json!({"preset": "Ring"})).unwrap();
    assert!(s.execute("shape.presets.place", json!({"preset": "Unicorn"})).is_err());
    assert!(s.execute("shape.presets.place", json!({"preset": "Heart", "rect": [0, 0, 0, 5]})).is_err());
    // Undo removes the layer.
    let n = s.active().unwrap().doc.layers.len();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.layers.len(), n - 1);
}

#[test]
fn custom_shapes_join_the_shapes_panel() {
    let mut s = session(8);
    s.execute("shape.create", json!({"kind": "star", "rect": [0, 0, 30, 30]})).unwrap();
    s.execute("edit.defineCustomShape", json!({"name": "My Star"})).unwrap();
    let v = s.execute("shape.presets.list", json!({})).unwrap();
    let last = v["groups"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["name"], super::shapes::CUSTOM_GROUP);
    s.execute("shape.presets.place", json!({"preset": "My Star"})).unwrap();
    s.execute("shape.presets.edit", json!({"action": "rename", "preset": "My Star", "name": "Star 2"})).unwrap();
    s.execute("shape.presets.edit", json!({"action": "delete", "preset": "Star 2"})).unwrap();
    assert!(s.edit_state.custom_shapes.is_empty());
    // New preset from the active shape layer, normalised to the 100 box.
    s.execute("shape.presets.new", json!({"name": "From Layer", "group": "Mine"})).unwrap();
    let g = s.presets.shapes.iter().find(|g| g.name == "Mine").unwrap();
    let b = g.items[0].path.control_bounds().unwrap();
    assert!(b.2.max(b.3) > 99.9);
}

// ------------------------------------------------------------------ tool presets

#[test]
fn tool_presets_save_filter_select() {
    let mut s = Session::new();
    s.execute("tools.setBrush", json!({"size": 77.0})).unwrap();
    s.execute("tool.presets.new", json!({"name": "Seventy Seven", "tool": "brush", "includeColor": true, "options": {"toolOptions": {"x": 1}}})).unwrap();
    s.execute("tools.setBrush", json!({"size": 5.0})).unwrap();
    s.tools.foreground = [1.0, 0.0, 0.0, 1.0];
    let v = s.execute("tool.presets.list", json!({"tool": "eraser"})).unwrap();
    assert!(v["presets"].as_array().unwrap().iter().all(|p| p["tool"] == "eraser"));
    let r = s.execute("tool.presets.select", json!({"preset": "Seventy Seven"})).unwrap();
    assert_eq!(r["tool"], "brush");
    assert_eq!(r["options"]["toolOptions"]["x"], 1);
    // Options passed explicitly win over the current brush.
    assert!(r["options"]["brush"].is_null() || r["options"]["brush"].is_object());
    assert_eq!(s.tools.foreground, [0.0, 0.0, 0.0, 1.0]);
    s.execute("tool.presets.new", json!({"name": "Eraser 77", "tool": "eraser"})).unwrap();
    let r = s.execute("tool.presets.select", json!({"preset": "Eraser 77"})).unwrap();
    // Selecting "Seventy Seven" restored its 77 px brush, which the new preset captured.
    assert_eq!(r["options"]["brush"]["size"], json!(77.0));
    s.execute("tool.presets.select", json!({"preset": "Soft Round 100 px"})).unwrap();
    assert_eq!(s.tools.brush.size, 100.0);
    s.execute("tool.presets.edit", json!({"action": "rename", "preset": "Eraser 77", "name": "E"})).unwrap();
    s.execute("tool.presets.edit", json!({"action": "delete", "preset": "E"})).unwrap();
    assert!(s.execute("tool.presets.select", json!({"preset": "E"})).is_err());
    assert!(s.execute("tool.presets.edit", json!({"action": "newGroup"})).is_err());
    assert!(s.execute("tool.presets.new", json!({"name": "x"})).is_err());
}

/// Crop presets are `crop` tool presets: saved, listed, persisted and deleted like the others.
#[test]
fn crop_presets_are_tool_presets_that_persist() {
    let mut s = Session::new();
    let opts =
        json!({"toolOptions": {"crop_ratio": "whr", "crop_width": "4 in", "crop_height": "5 in", "crop_resolution": "300", "crop_resolution_unit": "px/in"}});
    s.execute("tool.presets.new", json!({"name": "4 x 5 in 300 ppi", "tool": "crop", "options": opts})).unwrap();
    let r = s.execute("tool.presets.new", json!({"name": "4 x 5 in 300 ppi", "tool": "crop", "options": opts})).unwrap();
    assert_eq!(r["name"], "4 x 5 in 300 ppi 2", "a taken name gets a number");
    let mut t = Session::new();
    t.load_prefs_json(&s.prefs_to_json()).unwrap();
    let v = t.execute("tool.presets.list", json!({"tool": "crop"})).unwrap();
    assert_eq!(v["presets"].as_array().unwrap().len(), 2);
    let r = t.execute("tool.presets.select", json!({"preset": "4 x 5 in 300 ppi"})).unwrap();
    assert_eq!(r["options"]["toolOptions"]["crop_width"], "4 in");
    // Deleting several is all or nothing.
    assert!(t.execute("tool.presets.edit", json!({"action": "delete", "preset": ["4 x 5 in 300 ppi", "missing"]})).is_err());
    assert_eq!(t.presets.tool_presets.iter().filter(|p| p.tool == "crop").count(), 2);
    let r = t.execute("tool.presets.edit", json!({"action": "delete", "preset": ["4 x 5 in 300 ppi 2", "4 x 5 in 300 ppi", "4 x 5 in 300 ppi"]})).unwrap();
    assert_eq!(r["deleted"], 2);
    assert!(t.presets.tool_presets.iter().all(|p| p.tool != "crop"));
}

#[test]
fn tool_preset_names_and_params_are_checked() {
    let mut s = Session::new();
    let n = s.presets.tool_presets.len();
    let long = "x".repeat(crate::brush_preset_cmds::MAX_NAME + 1);
    for bad in [
        json!({"name": long, "tool": "crop"}),
        json!({"name": 7, "tool": "crop"}),
        json!({"name": ["a"], "tool": "crop"}),
        json!({"tool": 3}),
        json!({"tool": "crop", "options": "big"}),
        json!({"tool": "crop", "options": [1]}),
        json!(null),
        json!([]),
    ] {
        assert!(s.execute("tool.presets.new", bad.clone()).is_err(), "{bad}");
    }
    assert_eq!(s.presets.tool_presets.len(), n);
    // An empty or blank name falls back to the tool's; the longest name fits.
    assert_eq!(s.execute("tool.presets.new", json!({"name": "   ", "tool": "crop"})).unwrap()["name"], "crop");
    let max = "é".repeat(crate::brush_preset_cmds::MAX_NAME);
    assert_eq!(s.execute("tool.presets.new", json!({"name": max, "tool": "crop"})).unwrap()["name"], json!(max));
    assert!(s.execute("tool.presets.edit", json!({"action": "rename", "preset": "crop", "name": long})).is_err());
    assert!(s.execute("tool.presets.edit", json!({"action": "rename", "preset": "crop", "name": "  "})).is_err());
    // The store is capped.
    while s.presets.tool_presets.len() < super::tools::MAX_PRESETS {
        let i = s.presets.tool_presets.len();
        s.presets.tool_presets.push(super::tools::ToolPreset { name: format!("p{i}"), tool: "crop".into(), options: json!({}) });
    }
    assert!(s.execute("tool.presets.new", json!({"name": "one more", "tool": "crop"})).is_err());
}

/// A damaged entry in the saved tool presets is skipped; the other presets (and preferences) load.
#[test]
fn damaged_tool_presets_skip_only_the_bad_entries() {
    let mut s = Session::new();
    s.execute("gradient.presets.new", json!({"name": "Kept", "stops": [[0, "#ff0000"], [1, "#0000ff"]]})).unwrap();
    let mut prefs: Value = serde_json::from_str(&s.prefs_to_json()).unwrap();
    prefs["presets"]["tool_presets"] = json!([
        {"name": "Good", "tool": "crop", "options": {"toolOptions": {"crop_ratio": "2:3"}}},
        {"name": 5, "tool": "crop"},
        {"tool": "crop"},
        {"name": "   ", "tool": "crop"},
        {"name": "x".repeat(300), "tool": "crop"},
        {"name": "No tool", "tool": ""},
        "junk",
        null,
        {"name": "Good", "tool": "brush"},
        {"name": "Odd options", "tool": "crop", "options": 12},
    ]);
    let mut t = Session::new();
    t.load_prefs_json(&prefs.to_string()).unwrap();
    let names: Vec<&str> = t.presets.tool_presets.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Good", "Odd options"]);
    assert!(t.presets.tool_presets[1].options.is_object());
    assert!(t.presets.gradients.iter().any(|g| g.items.iter().any(|p| p.name == "Kept")), "other presets still load");
    // Not an array at all: no tool presets, still no crash.
    prefs["presets"]["tool_presets"] = json!({"a": 1});
    let mut u = Session::new();
    u.load_prefs_json(&prefs.to_string()).unwrap();
    assert!(u.presets.tool_presets.is_empty());
}

// ------------------------------------------------------------------ clone source

fn clone_doc(depth: u32) -> Session {
    let mut s = session(depth);
    // A horizontal ramp in red, green = row, so transforms are measurable.
    s.edit("ramp", |doc, _| {
        let surf = doc.layers[0].surface_mut().unwrap();
        let fmt = surf.format();
        let mut px = Vec::new();
        for y in 0..48 {
            for x in 0..64 {
                px.extend(photocraft_raster::from_rgba(&fmt, [x as f32 / 63.0, y as f32 / 47.0, 0.0, 1.0]));
            }
        }
        surf.write_region(photocraft_geom::Rect::new(0, 0, 64, 48), &px);
        Ok(())
    })
    .unwrap();
    s
}

#[test]
fn clone_source_slots_and_identity_matches_offset() {
    for depth in [8, 16, 32] {
        let mut s = clone_doc(depth);
        s.execute("cloneSource.set", json!({"index": 2, "source": [10, 10]})).unwrap();
        let v = s.execute("cloneSource.list", json!({})).unwrap();
        assert_eq!(v["active"], 2);
        assert_eq!(v["sources"][2]["source"], json!([10.0, 10.0]));
        // Stroke at (40, 30) without params samples the active slot: offset (−30, −20).
        s.execute("paint.cloneStamp", json!({"points": [[40, 30]], "size": 5, "hardness": 100})).unwrap();
        let px = layer_px(&s, 40, 30);
        assert!((px[0] - 10.0 / 63.0).abs() < 0.01 && (px[1] - 10.0 / 47.0).abs() < 0.01, "depth {depth}: {px:?}");
        // Aligned: the pairing stuck, the next stroke keeps the offset.
        let v = s.execute("cloneSource.list", json!({})).unwrap();
        assert_eq!(v["sources"][2]["offset"], json!([-30.0, -20.0]));
        s.execute("paint.cloneStamp", json!({"points": [[50, 40]], "size": 3, "hardness": 100})).unwrap();
        let px = layer_px(&s, 50, 40);
        assert!((px[0] - 20.0 / 63.0).abs() < 0.01, "{px:?}");
    }
}

#[test]
fn clone_source_flip_scale_rotation() {
    let mut s = clone_doc(16);
    s.execute("cloneSource.set", json!({"source": [20, 20], "flipH": true})).unwrap();
    s.execute("paint.cloneStamp", json!({"points": [[40, 24]], "size": 21, "hardness": 100})).unwrap();
    // Flipped: 2 px right of the anchor samples 2 px left of the source.
    let px = layer_px(&s, 42, 24);
    assert!((px[0] - 18.0 / 63.0).abs() < 0.02, "{px:?}");
    s.execute("cloneSource.resetTransform", json!({})).unwrap();
    s.execute("cloneSource.set", json!({"source": [20, 20], "width": 200, "height": 200})).unwrap();
    let mut s2 = clone_doc(16);
    s2.presets.clone = s.presets.clone.clone();
    s2.execute("paint.cloneStamp", json!({"points": [[40, 24]], "size": 21, "hardness": 100})).unwrap();
    // 200 %: 4 px right of the anchor samples 2 px right of the source.
    let px = layer_px(&s2, 44, 24);
    assert!((px[0] - 22.0 / 63.0).abs() < 0.02, "{px:?}");
    // Rotation 90° (counter-clockwise on screen): 3 px right of the anchor comes from 3 px
    // below the source.
    let mut s3 = clone_doc(32);
    s3.execute("cloneSource.set", json!({"source": [20, 20], "rotation": 90})).unwrap();
    s3.execute("paint.cloneStamp", json!({"points": [[40, 24]], "size": 21, "hardness": 100})).unwrap();
    let px = layer_px(&s3, 43, 24);
    assert!((px[0] - 20.0 / 63.0).abs() < 0.02 && (px[1] - 23.0 / 47.0).abs() < 0.02, "{px:?}");
    // Explicit params override the slot; healing follows the slot too.
    let mut s4 = clone_doc(8);
    s4.execute("cloneSource.set", json!({"source": [20, 20], "rotation": 45})).unwrap();
    s4.execute("paint.healingBrush", json!({"points": [[40, 24]], "size": 7})).unwrap();
    s4.execute("paint.cloneStamp", json!({"points": [[10, 40]], "offset": [5, 0], "rotation": 0, "size": 3, "hardness": 100})).unwrap();
    let px = layer_px(&s4, 10, 40);
    assert!((px[0] - 15.0 / 63.0).abs() < 0.01, "{px:?}");
}

#[test]
fn clone_source_errors_and_overlay() {
    let mut s = clone_doc(8);
    assert!(s.execute("paint.cloneStamp", json!({"points": [[4, 4]]})).is_err());
    assert!(s.execute("cloneSource.select", json!({"index": 5})).is_err());
    assert!(s.execute("cloneSource.set", json!({"offset": [1, 1]})).is_err());
    let o = s.execute("cloneSource.overlay", json!({"opacity": 40, "clipped": false, "blend": "difference"})).unwrap();
    assert_eq!(o["opacity"], json!(40.0));
    assert!(s.execute("cloneSource.overlay", json!({"blend": "screen"})).is_err());
    let v: Value = s.execute("cloneSource.set", json!({"source": [1, 1], "width": 5000, "rotation": 270})).unwrap();
    assert_eq!(v["width"], json!(1000.0));
    assert_eq!(v["rotation"], json!(-90.0));
}

// ------------------------------------------------------------------ persistence

/// #5602: one damaged preset set in the preferences file must not discard the saved New
/// Document presets (or any other set) at the next launch.
#[test]
fn a_damaged_preset_set_keeps_the_other_sets() {
    let mut s = Session::new();
    let settings = json!({"width": 1600, "height": 900, "resolution": 144});
    s.execute("document.presets.save", json!({"name": "Banner", "settings": settings})).unwrap();
    let saved = s.presets.to_json(&s);
    for key in ["gradients", "gradient", "styles", "shapes", "pattern_groups", "custom_shapes", "layer_defaults", "swatches"] {
        let mut blob = saved.clone();
        blob[key] = json!("not a preset set");
        let mut next = Session::new();
        next.load_presets_json(blob);
        assert_eq!(next.presets.documents, s.presets.documents, "damaged `{key}`");
        assert_eq!(next.presets.tool_presets.len(), Session::new().presets.tool_presets.len(), "damaged `{key}`");
    }
    // Null sets keep their defaults, as before.
    let mut next = Session::new();
    next.load_presets_json(json!({"tool_presets": null, "gradients": null, "documents": saved["documents"].clone()}));
    assert_eq!(next.presets.tool_presets.len(), Session::new().presets.tool_presets.len());
    assert_eq!(next.presets.gradients.len(), Session::new().presets.gradients.len());
    assert_eq!(next.presets.documents, s.presets.documents);
}

// ------------------------------------------------------------------ export presets

#[test]
fn export_presets_survive_a_presets_json_round_trip() {
    let mut s = Session::new();
    s.execute("file.export.presets.save", json!({"name": "Web", "settings": {"format": "jpg", "quality": 70}})).unwrap();
    assert_eq!(s.presets.export_presets.len(), 1);

    let saved = s.presets.to_json(&s);
    assert_eq!(saved["export_presets"].as_array().map(Vec::len), Some(1));

    let mut restored = Session::new();
    restored.load_presets_json(saved);
    assert_eq!(restored.presets.export_presets.len(), 1);
    assert_eq!(restored.presets.export_presets[0].name, "Web");
    assert_eq!(restored.presets.export_presets[0].settings.format, "jpg");
}

// ------------------------------------------------------------------ color presets

/// A small RGB document with a warm background: adjustment layers are easy to see on it.
fn color_session() -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 24, "height": 18, "background": "#e04020"})).unwrap();
    s
}

fn adjustment_layer(name: &str, adjustment: Adjustment) -> Layer {
    Layer::new(name, LayerContent::Adjustment(adjustment))
}

/// Append a fixture layer without a history step or a preference change.
fn push_layer(s: &mut Session, layer: Layer) -> LayerId {
    let st = s.active_mut().unwrap();
    let id = layer.id;
    Arc::make_mut(&mut st.doc).layers.push(layer);
    id
}

fn color_pixel(s: &Session, x: i32, y: i32) -> [f32; 4] {
    let d = s.active().unwrap();
    photocraft_compose::render(&d.doc, photocraft_geom::Rect::from_xywh(x, y, 1, 1)).px[0]
}

fn history_steps(s: &Session) -> usize {
    s.active().unwrap().history.entries().len()
}

/// Writes an 8×8 solid PNG and returns its path.
fn solid_png(dir: &std::path::Path, name: &str, color: &str) -> std::path::PathBuf {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 8, "height": 8, "background": color})).unwrap();
    let path = dir.join(name);
    crate::file_cmds::save_doc(&s.active().unwrap().doc, path.to_str().unwrap(), None).unwrap();
    path
}

#[test]
fn color_presets_builtins_match_the_contract() {
    let mut s = Session::new();
    let v = s.execute("color.presets.list", json!({})).unwrap();
    let groups = v["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "only the built-in group starts out");
    assert_eq!(groups[0]["name"], "PhotoCraft");
    let presets = groups[0]["presets"].as_array().unwrap();
    let names: Vec<&str> = presets.iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Warm Portrait", "Cool Soft", "Punchy", "Faded Film", "Black & White"]);
    let expected = [
        (
            0,
            json!({"temperature": 25.0, "tint": -5.0, "exposure": 0.15, "contrast": 10.0, "highlights": -15.0, "shadows": 15.0, "whites": 0.0, "blacks": 0.0, "vibrance": 15.0, "saturation": 0.0}),
        ),
        (
            1,
            json!({"temperature": -20.0, "tint": 5.0, "exposure": 0.05, "contrast": -5.0, "highlights": -10.0, "shadows": 10.0, "whites": 0.0, "blacks": 5.0, "vibrance": 0.0, "saturation": -5.0}),
        ),
        (
            2,
            json!({"temperature": 5.0, "tint": 0.0, "exposure": 0.0, "contrast": 25.0, "highlights": -10.0, "shadows": 5.0, "whites": 10.0, "blacks": -10.0, "vibrance": 25.0, "saturation": 5.0}),
        ),
        (
            3,
            json!({"temperature": 10.0, "tint": 3.0, "exposure": 0.0, "contrast": -15.0, "highlights": -20.0, "shadows": 20.0, "whites": -5.0, "blacks": 15.0, "vibrance": -5.0, "saturation": -15.0}),
        ),
        (
            4,
            json!({"temperature": 0.0, "tint": 0.0, "exposure": 0.0, "contrast": 15.0, "highlights": 0.0, "shadows": 0.0, "whites": 0.0, "blacks": -5.0, "vibrance": 0.0, "saturation": -100.0}),
        ),
    ];
    for (i, params) in expected {
        let layers = presets[i]["layers"].as_array().unwrap();
        assert_eq!(layers.len(), 1, "{}", names[i]);
        assert_eq!(layers[0]["kind"], "basicColor");
        assert_eq!(layers[0]["name"], "Basic Color");
        assert_eq!(layers[0]["opacity"], json!(1.0));
        assert_eq!(layers[0]["blend"], "Normal");
        assert_eq!(layers[0]["params"], params, "{}", names[i]);
    }
}

#[test]
fn color_presets_new_captures_visible_adjustments_bottom_to_top() {
    let mut s = color_session();
    let basic = Adjustment::BasicColor {
        temperature: 12.0,
        tint: -8.0,
        exposure: 0.3,
        contrast: 5.0,
        highlights: -10.0,
        shadows: 20.0,
        whites: 4.0,
        blacks: -6.0,
        vibrance: 30.0,
        saturation: -12.0,
    };
    let mut root_basic = adjustment_layer("Root Basic", basic.clone());
    root_basic.opacity = 0.8;
    root_basic.blend = BlendMode::Multiply;
    root_basic.mask = Some(LayerMask::reveal_all());
    push_layer(&mut s, root_basic);

    let invert = Adjustment::Invert;
    let mut nested_invert = adjustment_layer("Nested Invert", invert.clone());
    nested_invert.opacity = 0.6;
    nested_invert.blend = BlendMode::Screen;
    let mut hidden_vibrance = adjustment_layer("Hidden Vibrance", Adjustment::Vibrance { vibrance: 20.0, saturation: -5.0 });
    hidden_vibrance.visible = false;
    push_layer(&mut s, Layer::group("Group One", vec![nested_invert, hidden_vibrance]));

    let mut hidden_exposure = adjustment_layer("Hidden Exposure", Adjustment::Exposure { exposure: 1.0, offset: 0.0, gamma: 1.0 });
    hidden_exposure.visible = false;
    push_layer(&mut s, hidden_exposure);

    let mut hidden_group = Layer::group("Hidden Group", vec![adjustment_layer("Inside Hidden", Adjustment::Threshold { level: 0.5 })]);
    hidden_group.visible = false;
    push_layer(&mut s, hidden_group);

    let top_vibrance = Adjustment::Vibrance { vibrance: 45.0, saturation: 10.0 };
    let mut top = adjustment_layer("Top Vibrance", top_vibrance.clone());
    top.opacity = 0.5;
    push_layer(&mut s, top);
    let fmt = s.active().unwrap().doc.pixel_format();
    push_layer(&mut s, Layer::raster("Pixels", fmt));

    assert_eq!(s.presets.color_presets.len(), 1, "no user group yet");
    let rev = s.presets.rev;
    let steps = history_steps(&s);
    let r = s.execute("color.presets.new", json!({"name": "  Look A  "})).unwrap();
    assert_eq!(r["name"], "Look A", "name is trimmed");
    assert_eq!(r["group"], "My Presets", "the default group is created");
    assert_eq!(r["layers"], 3);
    assert_eq!(s.presets.rev, rev + 1, "a capture bumps the preset revision");
    assert_eq!(history_steps(&s), steps, "a capture does not touch the document history");

    let group = s.presets.color_presets.iter().find(|g| g.name == "My Presets").unwrap();
    assert_eq!(group.items.len(), 1);
    let preset = &group.items[0];
    assert_eq!(preset.name, "Look A");
    let kinds: Vec<&str> = preset.layers.iter().map(|l| l.kind.as_str()).collect();
    assert_eq!(kinds, ["basicColor", "invert", "vibrance"], "visible layers, bottom-to-top");
    let names: Vec<&str> = preset.layers.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["Root Basic", "Nested Invert", "Top Vibrance"]);
    assert_eq!(preset.layers[0].params, crate::adjust_params::to_params(&basic));
    assert_eq!(preset.layers[1].params, crate::adjust_params::to_params(&invert));
    assert_eq!(preset.layers[2].params, crate::adjust_params::to_params(&top_vibrance));
    assert_eq!(preset.layers[0].opacity, 0.8);
    assert_eq!(preset.layers[1].opacity, 0.6);
    assert_eq!(preset.layers[2].opacity, 0.5);
    assert_eq!(preset.layers[0].blend, "Multiply");
    assert_eq!(preset.layers[1].blend, "Screen");
    assert_eq!(preset.layers[2].blend, "Normal");
    // Only the five look fields are stored: no mask, effects, ids or pixels.
    for l in &preset.layers {
        let v = l.to_json();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["blend", "kind", "name", "opacity", "params"]);
    }

    // A duplicate in the same group gets a number; another group may reuse the name.
    let r = s.execute("color.presets.new", json!({"name": "Look A"})).unwrap();
    assert_eq!(r["name"], "Look A 2");
    s.execute("color.presets.edit", json!({"action": "newGroup", "name": "Other"})).unwrap();
    let r = s.execute("color.presets.new", json!({"name": "Look A", "group": "  Other  "})).unwrap();
    assert_eq!(r["group"], "Other");
    assert_eq!(r["name"], "Look A", "uniqueness is per group");
    // An omitted name takes the next free automatic one.
    let r = s.execute("color.presets.new", json!({})).unwrap();
    assert_eq!(r["name"], "Look 1");
    assert_eq!(history_steps(&s), steps, "captures never edit the document");
}

#[test]
fn color_presets_new_needs_a_visible_supported_adjustment() {
    let mut s = color_session();
    let before = s.presets.color_presets.clone();
    for p in [json!({"name": "Empty"}), json!({})] {
        let e = s.execute("color.presets.new", p.clone()).unwrap_err();
        assert!(e.to_string().contains("no visible adjustment layers"), "{p}: {e}");
    }
    let mut hidden = adjustment_layer("Hidden", Adjustment::Invert);
    hidden.visible = false;
    push_layer(&mut s, hidden);
    assert!(s.execute("color.presets.new", json!({"name": "Hidden"})).is_err());
    let mut hidden_group = Layer::group("Hidden Group", vec![adjustment_layer("Visible Inside", Adjustment::Invert)]);
    hidden_group.visible = false;
    push_layer(&mut s, hidden_group);
    assert!(s.execute("color.presets.new", json!({"name": "Hidden Group"})).is_err());
    // Imported unsupported adjustments have no parameter set to recreate them from.
    push_layer(&mut s, adjustment_layer("Imported", Adjustment::Unsupported { psd_key: "test".into(), raw: vec![1, 2, 3] }));
    assert!(s.execute("color.presets.new", json!({"name": "Unsupported"})).is_err());
    assert_eq!(s.presets.color_presets, before, "failed captures add nothing");
    let mut t = Session::new();
    assert!(t.execute("color.presets.new", json!({})).is_err(), "no document");
}

#[test]
fn color_presets_apply_creates_one_pass_through_group_undo_step() {
    let mut s = color_session();
    s.execute("layer.new.layer", json!({"name": "Ink"})).unwrap();
    let before_layers = s.active().unwrap().doc.layers.clone();
    let before_px = color_pixel(&s, 6, 6);
    assert!(before_px[0] > before_px[1] && before_px[0] > 0.7, "warm red background: {before_px:?}");
    let steps = history_steps(&s);

    let r = s.execute("color.presets.apply", json!({"preset": "Black & White", "group": "PhotoCraft"})).unwrap();
    assert_eq!(r["name"], "Black & White");
    let gid = LayerId(r["group"].as_u64().unwrap());
    let d = s.active().unwrap();
    assert_eq!(d.doc.layers.len(), before_layers.len() + 1, "one new top-level group");
    assert_eq!(&d.doc.layers[..before_layers.len()], &before_layers[..], "existing layers are untouched");
    let group = d.doc.layers.last().unwrap();
    assert_eq!(group.id, gid);
    assert_eq!(group.name, "Preset \u{2022} Black & White");
    assert_eq!(group.blend, BlendMode::PassThrough, "the look must reach the composite below");
    assert_eq!(group.opacity, 1.0);
    assert!(group.visible && group.mask.is_none());
    let LayerContent::Group(g) = &group.content else { panic!("not a group") };
    assert!(g.expanded, "the new group is expanded");
    assert_eq!(g.children.len(), 1);
    let child = &g.children[0];
    assert_eq!(child.id.0, r["layers"][0].as_u64().unwrap());
    assert_eq!(child.name, "Basic Color");
    assert_eq!(child.opacity, 1.0);
    assert_eq!(child.blend, BlendMode::Normal);
    assert!(child.visible && child.mask.is_none(), "applied looks are global");
    assert!(matches!(child.content, LayerContent::Adjustment(Adjustment::BasicColor { saturation, .. }) if (saturation + 100.0).abs() < 1e-5));
    assert_eq!(d.active_layer, Some(gid), "the new group is selected");
    assert_eq!(d.history.entries().len(), steps + 1, "exactly one undo step");
    assert_eq!(d.history.entries().last().map(String::as_str), Some("Apply Preset \u{201c}Black & White\u{201d}"));

    let after_px = color_pixel(&s, 6, 6);
    assert!((after_px[0] - after_px[1]).abs() < 0.02 && (after_px[1] - after_px[2]).abs() < 0.02, "desaturated: {after_px:?}");
    assert!((after_px[0] - before_px[0]).abs() > 0.1, "the composite changed");

    // Undo removes the whole group in one step; redo restores it with the same ids.
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.layers, before_layers);
    s.execute("edit.redo", json!({})).unwrap();
    let d = s.active().unwrap();
    assert_eq!(d.doc.layers.len(), before_layers.len() + 1);
    assert_eq!(d.doc.layers.last().unwrap().id, gid);
    assert_eq!(d.doc.layers.last().unwrap().children().unwrap().len(), 1);

    // Applying again stacks another group; the first one and the existing layers stay as they were.
    let r2 = s.execute("color.presets.apply", json!({"preset": "Warm Portrait"})).unwrap();
    assert_ne!(r2["group"], r["group"]);
    let d = s.active().unwrap();
    assert_eq!(d.doc.layers.len(), before_layers.len() + 2);
    assert_eq!(&d.doc.layers[..before_layers.len()], &before_layers[..]);
    assert_eq!(d.doc.layers.last().unwrap().id.0, r2["group"].as_u64().unwrap());
    assert_eq!(d.doc.layers[d.doc.layers.len() - 2].id, gid);
    assert_eq!(d.doc.layers.last().unwrap().name, "Preset \u{2022} Warm Portrait");
}

#[test]
fn color_presets_apply_inline_and_rejects_bad_presets_atomically() {
    let mut s = color_session();
    let steps = history_steps(&s);
    let r = s
        .execute(
            "color.presets.apply",
            json!({"preset": {"name": "Inline Invert", "layers": [
                {"kind": "invert", "name": "", "params": {}, "opacity": 2.0, "blend": "Normal"}
            ]}}),
        )
        .unwrap();
    assert_eq!(r["name"], "Inline Invert");
    let d = s.active().unwrap();
    let group = d.doc.layers.last().unwrap();
    assert_eq!(group.name, "Preset \u{2022} Inline Invert");
    let child = &group.children().unwrap()[0];
    assert_eq!(child.name, "Invert", "an empty name falls back to the kind label");
    assert_eq!(child.opacity, 1.0, "opacity is clamped");
    assert_eq!(d.history.entries().len(), steps + 1);
    let px = color_pixel(&s, 6, 6);
    assert!(px[0] < 0.2 && px[1] > 0.7 && px[2] > 0.8, "red inverted: {px:?}");
    s.execute("edit.undo", json!({})).unwrap();
    let base = s.active().unwrap().doc.layers.clone();
    let base_steps = history_steps(&s);

    // Multi-layer presets keep their stored order, opacity and blend inside the new group.
    let r = s
        .execute(
            "color.presets.apply",
            json!({"preset": {"name": "Stacked", "layers": [
                {"kind": "invert", "name": "Bottom", "params": {}, "opacity": 0.25, "blend": "Screen"},
                {"kind": "exposure", "name": "Top", "params": {"exposure": 1.0}, "opacity": 0.75, "blend": "Multiply"}
            ]}}),
        )
        .unwrap();
    let d = s.active().unwrap();
    let children = d.doc.layers.last().unwrap().children().unwrap();
    assert_eq!(children.len(), 2);
    assert_eq!(children[0].name, "Bottom");
    assert_eq!(children[0].opacity, 0.25);
    assert_eq!(children[0].blend, BlendMode::Screen);
    assert!(matches!(children[0].content, LayerContent::Adjustment(Adjustment::Invert)));
    assert_eq!(children[1].name, "Top");
    assert_eq!(children[1].opacity, 0.75);
    assert_eq!(children[1].blend, BlendMode::Multiply);
    assert!(matches!(children[1].content, LayerContent::Adjustment(Adjustment::Exposure { exposure, .. }) if (exposure - 1.0).abs() < 1e-6));
    let ids = r["layers"].as_array().unwrap();
    assert_eq!([ids[0].as_u64().unwrap(), ids[1].as_u64().unwrap()], [children[0].id.0, children[1].id.0], "bottom-to-top ids");
    assert_eq!(d.history.entries().len(), base_steps + 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.layers, base);
    assert_eq!(history_steps(&s), base_steps);

    let bad = [
        json!({"preset": {"name": "Bad kind", "layers": [{"kind": "nope", "params": {}}]}}),
        json!({"preset": {"name": "Bad params", "layers": [{"kind": "basicColor", "params": {"temperature": "hot"}}]}}),
        json!({"preset": {"name": "Bad blend", "layers": [{"kind": "invert", "params": {}, "blend": "Nope"}]}}),
        json!({"preset": {"name": "Pass through", "layers": [{"kind": "invert", "params": {}, "blend": "PassThrough"}]}}),
        json!({"preset": {"name": "Empty", "layers": []}}),
        json!({"preset": {"name": "   ", "layers": [{"kind": "invert", "params": {}}]}}),
        json!({"preset": 7}),
        json!({}),
        json!({"preset": "No Such Look"}),
        json!({"preset": "Warm Portrait", "group": "No Such Group"}),
    ];
    for p in bad {
        let e = s.execute("color.presets.apply", p.clone()).unwrap_err();
        assert!(!e.to_string().is_empty(), "{p}");
        assert_eq!(s.active().unwrap().doc.layers, base, "no partial stack for {p}");
        assert_eq!(history_steps(&s), base_steps, "no undo step for {p}");
    }
    let mut t = Session::new();
    assert!(t.execute("color.presets.apply", json!({"preset": "Warm Portrait"})).is_err(), "no document");
}

#[test]
fn color_presets_list_and_edit_actions() {
    let mut s = color_session();
    s.execute("layer.newAdjustmentLayer.basicColor", json!({"temperature": 10})).unwrap();
    s.execute("color.presets.new", json!({"name": "Take"})).unwrap();
    let v = s.execute("color.presets.list", json!({})).unwrap();
    let g = v["groups"].as_array().unwrap().iter().find(|g| g["name"] == "My Presets").unwrap();
    assert_eq!(g["presets"][0]["name"], "Take");
    assert_eq!(g["presets"][0]["layers"][0]["params"]["temperature"], json!(10.0));

    let rev = s.presets.rev;
    s.execute("color.presets.edit", json!({"action": "rename", "preset": "Take", "name": "Renamed"})).unwrap();
    assert_eq!(s.presets.rev, rev + 1, "edits bump the revision");
    let r = s.execute("color.presets.edit", json!({"action": "rename", "preset": "Renamed", "name": "Warm Portrait"})).unwrap_err();
    assert!(r.to_string().contains("already exists"), "{r}");
    assert_eq!(s.presets.rev, rev + 1, "a failed edit changes nothing");
    assert!(s.execute("color.presets.edit", json!({"action": "nope"})).is_err());

    s.execute("color.presets.edit", json!({"action": "newGroup", "name": "Mine"})).unwrap();
    s.execute("color.presets.edit", json!({"action": "newGroup", "name": "Mine"})).unwrap();
    assert!(s.presets.color_presets.iter().any(|g| g.name == "Mine 1"), "unique group names");
    s.execute("color.presets.edit", json!({"action": "move", "preset": "Renamed", "to": "Mine", "index": 0})).unwrap();
    let mine = s.presets.color_presets.iter().find(|g| g.name == "Mine").unwrap();
    assert_eq!(mine.items[0].name, "Renamed");
    s.execute("color.presets.edit", json!({"action": "renameGroup", "group": "Mine", "name": "Mine Renamed"})).unwrap();
    assert!(s.presets.color_presets.iter().any(|g| g.name == "Mine Renamed"));
    let r = s.execute("color.presets.edit", json!({"action": "delete", "preset": ["Renamed", "missing"]})).unwrap_err();
    assert!(r.to_string().contains("no preset"), "{r}");
    let r = s.execute("color.presets.edit", json!({"action": "delete", "preset": "Renamed"})).unwrap();
    assert_eq!(r["deleted"], 1);
    let r = s.execute("color.presets.edit", json!({"action": "deleteGroup", "group": "Mine Renamed"})).unwrap();
    assert_eq!(r["deleted"], 0);
    assert!(s.execute("color.presets.edit", json!({"action": "deleteGroup", "group": "Missing"})).is_err());
}

#[test]
fn color_presets_persist_independently_and_load_is_not_an_edit() {
    let mut s = Session::new();
    s.execute("gradient.presets.new", json!({"name": "Keep Me", "stops": [[0, "#ff0000"], [1, "#0000ff"]]})).unwrap();
    s.execute("file.new", json!({"width": 16, "height": 16})).unwrap();
    s.execute("layer.newAdjustmentLayer.invert", json!({})).unwrap();
    s.execute("color.presets.new", json!({"name": "Saved Look", "group": "Mine"})).unwrap();
    let saved = s.presets.to_json(&s);
    assert_eq!(saved["color_presets"].as_array().unwrap().len(), 2, "PhotoCraft + Mine");

    let mut t = Session::new();
    let rev = t.presets.rev;
    t.load_presets_json(saved.clone());
    assert_eq!(t.presets.rev, rev, "loading is not an edit");
    assert!(t.presets.color_presets.iter().any(|g| g.name == "Mine" && g.items[0].name == "Saved Look"));
    assert_eq!(t.presets.color_presets[0].items.len(), 5, "built-ins round-trip");

    // Deleting a built-in persists: it must not reappear on the next launch.
    s.execute("color.presets.edit", json!({"action": "delete", "preset": "Warm Portrait"})).unwrap();
    let saved = s.presets.to_json(&s);
    let mut u = Session::new();
    u.load_presets_json(saved.clone());
    assert!(!u.presets.color_presets.iter().flat_map(|g| &g.items).any(|p| p.name == "Warm Portrait"));
    assert_eq!(u.presets.color_presets[0].items.len(), 4);

    // Missing or malformed sets keep the defaults; an explicitly empty list stays empty.
    let mut m = Session::new();
    m.load_presets_json(json!({"gradients": []}));
    assert_eq!(m.presets.color_presets[0].items.len(), 5);
    let mut bad = Session::new();
    bad.load_presets_json(json!({"color_presets": "not a set"}));
    assert_eq!(bad.presets.color_presets[0].items.len(), 5);
    let mut empty = Session::new();
    empty.load_presets_json(json!({"color_presets": []}));
    assert!(empty.presets.color_presets.is_empty(), "an emptied library stays empty");

    // Damaged entries and groups are dropped one by one.
    let mut partial = Session::new();
    partial.load_presets_json(json!({"color_presets": [
        {"name": "Good Group", "items": [
            {"name": "Good", "layers": [{"kind": "invert", "name": "", "params": {}, "opacity": 1.0, "blend": "Normal"}]},
            {"name": 5},
            "junk"
        ]},
        {"name": "   ", "items": []},
        {"items": []},
        "junk"
    ]}));
    assert_eq!(partial.presets.color_presets.len(), 1);
    let group = &partial.presets.color_presets[0];
    assert_eq!(group.name, "Good Group");
    assert_eq!(group.items.len(), 1);
    assert_eq!(group.items[0].name, "Good");
    assert_eq!(group.items[0].layers.len(), 1);

    // A damaged colour set does not discard the other preset kinds, and vice versa.
    let mut blob = saved.clone();
    blob["color_presets"] = json!({"broken": true});
    let mut other = Session::new();
    other.load_presets_json(blob);
    assert!(other.presets.gradients.iter().any(|g| g.items.iter().any(|p| p.name == "Keep Me")), "gradients still load");
    assert_eq!(other.presets.color_presets[0].items.len(), 5, "colour defaults");
    let mut blob = saved.clone();
    blob["gradients"] = json!("broken");
    let mut color = Session::new();
    color.load_presets_json(blob);
    assert!(color.presets.color_presets.iter().any(|g| g.name == "Mine"), "colour presets still load");
    assert_eq!(color.presets.gradients, Session::new().presets.gradients, "gradients keep their defaults");
}

#[test]
fn color_presets_batch_applies_a_user_preset_to_a_folder() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in");
    let output = dir.path().join("out");
    std::fs::create_dir_all(&input).unwrap();
    std::fs::create_dir_all(&output).unwrap();
    let sources = [("a.png", "#ff0000"), ("b.png", "#00ff00"), ("c.png", "#0000ff")];
    let before: Vec<(std::path::PathBuf, Vec<u8>)> = sources
        .iter()
        .map(|(name, color)| {
            let p = solid_png(&input, name, color);
            let bytes = std::fs::read(&p).unwrap();
            (p, bytes)
        })
        .collect();
    let broken = input.join("broken.png");
    std::fs::write(&broken, b"not a png").unwrap();
    let broken_bytes = std::fs::read(&broken).unwrap();

    // The preset lives in the caller's library and is addressed by name, as the panel does.
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
    s.execute("layer.newAdjustmentLayer.invert", json!({})).unwrap();
    s.execute("color.presets.new", json!({"name": "Batch Invert", "group": "My Presets"})).unwrap();

    let r = s
        .execute(
            "file.automate.batch",
            json!({
                "steps": [["color.presets.apply", {"preset": "Batch Invert", "group": "My Presets"}]],
                "input": input.to_str().unwrap(),
                "output": output.to_str().unwrap(),
                "format": "png",
                "includeSubfolders": false
            }),
        )
        .unwrap();
    assert_eq!(r["files"].as_array().unwrap().len(), 3, "{r}");
    let errors = r["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "{r}");
    assert!(errors[0]["file"].as_str().unwrap().ends_with("broken.png"), "{r}");

    for (path, bytes) in &before {
        let name = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(&std::fs::read(path).unwrap(), bytes, "{name}: source untouched");
        let out = output.join(name);
        assert!(out.is_file(), "{name}: output written");
        let src = photocraft_io::import(name, bytes).unwrap().document;
        let dst = photocraft_io::import(name, &std::fs::read(&out).unwrap()).unwrap().document;
        let rect = photocraft_geom::Rect::from_xywh(4, 4, 1, 1);
        let a = photocraft_compose::render(&src, rect).px[0];
        let b = photocraft_compose::render(&dst, rect).px[0];
        assert!((a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs() > 0.5, "{name}: pixels differ ({a:?} vs {b:?})");
    }
    // Red becomes cyan through the applied invert.
    let a = photocraft_io::import("a.png", &std::fs::read(output.join("a.png")).unwrap()).unwrap().document;
    let px = photocraft_compose::render(&a, photocraft_geom::Rect::from_xywh(4, 4, 1, 1)).px[0];
    assert!(px[0] < 0.02 && px[1] > 0.98 && px[2] > 0.98, "{px:?}");
    assert_eq!(std::fs::read(&broken).unwrap(), broken_bytes, "the bad file is untouched");
    assert_eq!(std::fs::read_dir(&output).unwrap().count(), 3, "no output for the bad file");
}
