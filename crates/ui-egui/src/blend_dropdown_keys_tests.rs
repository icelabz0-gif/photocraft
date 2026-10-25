//! #1318: the Layers panel's Blend Mode dropdown takes the arrow keys once it is open, ahead of
//! the tool shortcuts (the Move tool's arrow-key nudge).

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use photocraft_doc::BlendMode;
use serde_json::json;

use crate::PhotocraftApp;
use crate::state::Tool;

#[test]
fn arrow_keys_step_the_open_blend_mode_dropdown_without_nudging_the_layer() {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 520.0)).with_max_steps(64).build_eframe(|cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 64, "height": 64})).unwrap();
        app.run("layer.new.layer", json!({})).unwrap();
        app.run("edit.fill", json!({"color": "#ff0000"})).unwrap();
        app.ui.tool = Tool::Move;
        app
    });
    h.run_steps(6);
    let layer = |h: &Harness<'_, PhotocraftApp>| {
        let st = h.state().session.active().unwrap();
        let l = st.doc.layer(st.active_layer.unwrap()).unwrap();
        (l.blend, l.surface().unwrap().content_bounds())
    };
    let (blend, bounds) = layer(&h);
    assert_eq!(blend, BlendMode::Normal);
    let named = |n: &egui_kittest::Node<'_>| {
        let a = n.accesskit_node();
        a.value().as_deref() == Some("Normal") || a.label().as_deref() == Some("Normal")
    };
    let combo = h.query_all_by_role(Role::ComboBox).find(|n| named(n));
    combo.expect("the Layers panel's Blend Mode dropdown").click();
    h.run_steps(2);
    h.key_press(egui::Key::ArrowDown);
    h.run_steps(2);
    let (blend, moved) = layer(&h);
    assert_ne!(blend, BlendMode::Normal, "↓ picks the next mode");
    assert_eq!(moved, bounds, "the Move tool's nudge doesn't also take the key");
    h.key_press(egui::Key::ArrowUp);
    h.run_steps(2);
    assert_eq!(layer(&h).0, BlendMode::Normal);
    // Close the list and open it again with the pointer, which then rests on the list.
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    let combo = h.query_all_by_role(Role::ComboBox).find(|n| named(n)).expect("the dropdown again");
    let r = combo.accesskit_node().raw_bounds().expect("its bounds");
    let at = egui::pos2(((r.x0 + r.x1) / 2.0) as f32, ((r.y0 + r.y1) / 2.0) as f32);
    h.event(egui::Event::PointerMoved(at));
    h.run_steps(2);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
        h.run_steps(2);
    }
    assert!(crate::widgets::dropdown_list_open(&h.ctx), "the click opens the list");
    h.event(egui::Event::PointerMoved(at + egui::vec2(0.0, 80.0)));
    h.run_steps(2);
    // Every press steps again, not just the first, and keyboard focus stays on the dropdown
    // instead of moving to the Lock icons below it (#9334). A keyboard's press and release come
    // in separate frames.
    let key = |h: &mut Harness<'_, PhotocraftApp>, key: egui::Key| {
        for pressed in [true, false] {
            h.event(egui::Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Default::default() });
            h.run_steps(2);
        }
    };
    let focused = h.ctx.memory(|m| m.focused());
    assert!(focused.is_some(), "the click focuses the dropdown");
    let mut seen = vec![BlendMode::Normal];
    for _ in 0..3 {
        key(&mut h, egui::Key::ArrowDown);
        seen.push(layer(&h).0);
        assert_eq!(h.ctx.memory(|m| m.focused()), focused, "focus stays on the dropdown");
    }
    seen.dedup();
    assert_eq!(seen.len(), 4, "each ↓ picks the next mode: {seen:?}");
    for _ in 0..3 {
        key(&mut h, egui::Key::ArrowUp);
    }
    assert_eq!(layer(&h).0, BlendMode::Normal, "↑ steps back each time");
    assert_eq!(layer(&h).1, bounds, "the Move tool's nudge never takes the keys");
}

/// #1481: with the Blend Mode list open, a letter picks the next mode starting with it (as in
/// Photoshop's pop-up menus) instead of reaching the tool shortcuts, and ↩ closes the list.
#[test]
fn letters_pick_blend_modes_in_the_open_list_without_switching_tools() {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_max_steps(64).build_eframe(|cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 64, "height": 64})).unwrap();
        app.run("layer.new.layer", json!({})).unwrap();
        app.ui.tool = Tool::Move;
        app
    });
    h.run_steps(6);
    let blend = |h: &Harness<'_, PhotocraftApp>| {
        let st = h.state().session.active().unwrap();
        st.doc.layer(st.active_layer.unwrap()).unwrap().blend
    };
    let type_key = |h: &mut Harness<'_, PhotocraftApp>, key: egui::Key, text: &str| {
        h.key_press(key);
        h.event(egui::Event::Text(text.into()));
        h.run_steps(2);
    };
    let named = |n: &egui_kittest::Node<'_>| {
        let a = n.accesskit_node();
        a.value().as_deref() == Some("Normal") || a.label().as_deref() == Some("Normal")
    };
    h.query_all_by_role(Role::ComboBox).find(|n| named(n)).expect("the Layers panel's Blend Mode dropdown").click();
    h.run_steps(2);
    type_key(&mut h, egui::Key::M, "m");
    assert_eq!(blend(&h), BlendMode::Multiply, "M picks Multiply");
    assert_eq!(h.state().ui.tool, Tool::Move, "M doesn't also pick the Marquee tool");
    type_key(&mut h, egui::Key::D, "d");
    assert_eq!(blend(&h), BlendMode::DarkerColor, "D picks the next mode starting with D");
    type_key(&mut h, egui::Key::D, "d");
    assert_eq!(blend(&h), BlendMode::Difference, "D again picks the one after");
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert!(!crate::widgets::dropdown_list_open(&h.ctx), "↩ closes the list");
    assert_eq!(blend(&h), BlendMode::Difference, "↩ keeps the mode");
    // With the list closed the letters are the tools' again.
    h.key_press(egui::Key::M);
    h.event(egui::Event::Text("m".into()));
    h.run_steps(2);
    assert!(h.state().ui.tool != Tool::Move, "M picks a marquee tool once the list is closed");
}
