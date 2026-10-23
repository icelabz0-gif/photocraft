//! Pointer, keyboard, colour precision and history regressions for shape appearance controls.
use super::*;
use egui::{Event, Key, Modifiers};
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use photocraft_doc::ShapeLayer;

fn app(mode: &str, depth: u32) -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
    app.run("file.new", json!({"width": 120, "height": 100, "mode": mode, "depth": depth})).unwrap();
    app.ui.tool = Tool::Rectangle;
    app.sync_views();
    app
}

fn shape(app: &PhotocraftApp) -> &ShapeLayer {
    let st = app.session.active().unwrap();
    let LayerContent::Shape(shape) = &st.doc.layer(st.active_layer.unwrap()).unwrap().content else { panic!("expected editable shape") };
    shape
}

fn assert_saved_path(saved: &Path, original: &Path) {
    assert_eq!((saved.fill_rule, saved.inverted), (original.fill_rule, original.inverted));
    assert_eq!(saved.subpaths.len(), original.subpaths.len());
    for (saved, original) in saved.subpaths.iter().zip(&original.subpaths) {
        assert_eq!((saved.closed, saved.op, saved.knots.len()), (original.closed, original.op, original.knots.len()));
        for (saved, original) in saved.knots.iter().zip(&original.knots) {
            assert_eq!(saved.smooth, original.smooth);
            for (saved, original) in [(saved.anchor, original.anchor), (saved.in_ctrl, original.in_ctrl), (saved.out_ctrl, original.out_ctrl)] {
                // Native manifests use JSON f64 coordinates, whose existing round trip may
                // differ by one ulp for generated ellipse/polygon control points.
                assert!((saved.x - original.x).abs() <= 1e-10 && (saved.y - original.y).abs() <= 1e-10, "{saved:?} differs from {original:?}");
            }
        }
    }
}

fn harness(app: PhotocraftApp, properties: bool) -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(vec2(1000.0, 700.0)).build_ui_state(
        move |ui, app: &mut PhotocraftApp| {
            crate::screen_picker::tick(app, ui.ctx());
            if !ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                return;
            }
            if properties {
                let id = app.session.active().unwrap().active_layer.unwrap();
                ui.set_max_width(320.0);
                shape_properties(app, ui, id);
            } else {
                let tool = app.ui.tool;
                ui.horizontal(|ui| options_bar(app, ui, tool));
            }
            // The Color Picker the popover opens, as the app draws it.
            crate::dialogs::show(app, ui.ctx());
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::Pro);
    h.run_steps(3);
    h
}

fn stroke_width(h: &mut Harness<'_, PhotocraftApp>, width: &str) {
    h.get_all_by_role(egui::accesskit::Role::SpinButton).next().expect("the stroke width").click();
    h.run();
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.event(Event::Text(width.into()));
    h.key_press(Key::Enter);
    h.run_steps(3);
}

/// #6191: passing through zero hides the stroke without discarding its paint or style.
#[test]
fn zero_width_preserves_selected_shape_stroke_and_restores_it() {
    for depth in [8, 16, 32] {
        for (properties, tool) in [(true, Tool::Rectangle), (false, Tool::Rectangle), (false, Tool::PathSelection), (false, Tool::DirectSelection)] {
            let mut app = app("rgb", depth);
            app.ui.tool = tool;
            app.run(
                "shape.create",
                json!({"rect": [20, 20, 80, 60], "fill": null,
                    "stroke": {"width": 4, "color": [0.123456, 0.345678, 0.567891, 0.618237],
                        "align": "inside", "cap": "round", "join": "bevel", "miterLimit": 7,
                        "dashes": [3, 2], "dashOffset": 1.25, "opacity": 63}}),
            )
            .unwrap();
            let original = shape(&app).clone();
            let history = app.session.active().unwrap().history.past_len();
            let mut h = harness(app, properties);
            stroke_width(&mut h, "0");
            let zero = shape(h.state()).clone();
            let mut expected = original.stroke.clone().unwrap();
            expected.width = 0.0;
            assert_eq!(zero.stroke, Some(expected), "{tool:?}, Properties={properties}, depth={depth}: zero width retains the stroke");
            assert_eq!(zero.path, original.path);
            assert!(zero.cache.as_ref().unwrap().content_bounds().is_empty(), "a zero-width stroke paints nothing");
            assert_eq!(h.state().session.active().unwrap().history.past_len(), history + 1);

            // A save at zero must retain the dormant stroke too.
            let st = h.state().session.active().unwrap();
            let bytes = photocraft_format::save_to_bytes(&st.doc, &Default::default()).unwrap();
            let loaded = photocraft_format::load_from_bytes(&bytes).unwrap();
            let LayerContent::Shape(saved) = &loaded.layer(st.active_layer.unwrap()).unwrap().content else { panic!("saved shape") };
            assert_eq!(saved.stroke, zero.stroke);

            stroke_width(&mut h, "4");
            assert_eq!(shape(h.state()), &original, "restoring width restores the complete style and rendered pixels");
            h.state_mut().run("edit.undo", json!({})).unwrap();
            assert_eq!(shape(h.state()), &zero);
            h.state_mut().run("edit.undo", json!({})).unwrap();
            assert_eq!(shape(h.state()), &original);
            h.state_mut().run("edit.redo", json!({})).unwrap();
            assert_eq!(shape(h.state()), &zero);
            h.state_mut().run("edit.redo", json!({})).unwrap();
            assert_eq!(shape(h.state()), &original);
        }
    }
}

#[test]
fn zero_width_style_can_be_inspected_removed_or_reenabled_explicitly() {
    for properties in [true, false] {
        let mut app = app("rgb", 8);
        app.run("shape.create", json!({"rect": [20, 20, 80, 60], "stroke": {"width": 4, "color": "#e73581", "dashes": [3, 2]}})).unwrap();
        let paint = shape(&app).stroke.as_ref().unwrap().paint.clone();
        let mut h = harness(app, properties);
        stroke_width(&mut h, "0");
        let zero = shape(h.state()).clone();
        h.get_by_label("Stroke Options").click();
        h.run_steps(3);
        assert_eq!(h.state().ui.stroke_editor.as_ref().unwrap().draft.dashes, [3.0, 2.0], "zero width still exposes the saved options");
        h.key_press(Key::Escape);
        h.run_steps(2);
        assert_eq!(shape(h.state()), &zero, "merely inspecting the style does not enable it");

        h.get_by_label(if properties { "Set shape stroke type" } else { "Stroke color" }).click();
        h.run_steps(3);
        h.get_by_label("No Color").click();
        h.run_steps(2);
        assert!(shape(h.state()).stroke.is_none(), "No Color still removes the stroke");
        h.key_press(Key::Escape);
        h.run_steps(2);
        h.state_mut().run("edit.undo", json!({})).unwrap();
        assert_eq!(shape(h.state()), &zero);
        h.run_steps(2);

        h.get_by_label("Stroke Options").click();
        h.run_steps(3);
        h.get_by_role_and_label(egui::accesskit::Role::RadioButton, "Dotted").click();
        h.run_steps(3);
        let stroke = shape(h.state()).stroke.as_ref().unwrap();
        assert_eq!(stroke.width, 3.0, "a deliberate style choice enables the dormant stroke");
        assert_eq!(stroke.paint, paint);
        assert_eq!(stroke.dashes, [0.0, 2.0]);
    }
}

#[test]
fn fill_and_stroke_swatches_open_with_pointer_and_keyboard_without_changing_paint() {
    for label in ["Fill color", "Stroke color"] {
        let mut app = app("rgb", 32);
        let foreground = [0.123456, 0.345678, 0.567891, 0.372819];
        let background = [0.891234, 0.678912, 0.456789, 0.0];
        app.run("tools.setColors", json!({"foreground": foreground, "background": background})).unwrap();
        let document = app.session.active().unwrap().doc.clone();
        let mut h = harness(app, false);
        h.get_by_label(label).click();
        h.run_steps(4);
        assert!(h.query_by_label("No Color").is_some(), "{label} opens its picker");
        assert_eq!(h.state().session.tools.foreground, foreground);
        assert_eq!(h.state().session.tools.background, background);
        assert_eq!(h.state().ui.tool_options.stroke_width, 0.0);
        assert_eq!(h.state().session.active().unwrap().doc, document);
        h.key_press(Key::Escape);
        h.run_steps(2);
        assert!(h.query_by_label("No Color").is_none());
        h.get_by_label(label).focus();
        h.run_steps(2);
        h.key_press(Key::Enter);
        h.run_steps(3);
        assert!(h.query_by_label("No Color").is_some(), "{label} opens from keyboard focus");
        h.key_press(Key::Escape);
        h.run_steps(2);
        assert_eq!(h.state().session.active().unwrap().doc, document);
        assert_eq!(h.state().session.tools.foreground, foreground);
        assert_eq!(h.state().session.tools.background, background);
    }
}

/// Open the popover's swatch folder `folder` and click its swatch `name`.
fn click_swatch(h: &mut Harness<'_, PhotocraftApp>, folder: &str, name: &str) {
    if h.query_by_label(name).is_none() {
        h.get_by_label(folder).click();
        h.run_steps(2);
    }
    h.get_by_label(name).click();
    h.run_steps(2);
}

/// Click the open Color Picker's hex field (found by the colour it shows) and select its text.
fn focus_hex(h: &mut Harness<'_, PhotocraftApp>) {
    let shown = h.state().ui.dialogs.last().expect("the Color Picker dialog").fields["color"].as_str().unwrap().trim_start_matches('#').to_ascii_uppercase();
    h.get_all_by_role(egui::accesskit::Role::TextInput).find(|n| n.value().as_deref() == Some(shown.as_str())).expect("the hex field").click();
    h.run_steps(1);
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run_steps(1);
}

/// The Color Picker opened from the popover keeps partial hex typing and the paint's alpha.
#[test]
fn shape_paint_hex_input_keeps_partial_text_and_preserves_alpha() {
    for label in ["Fill color", "Stroke color"] {
        let mut app = app("rgb", 32);
        let original = [0.123456, 0.345678, 0.567891, 0.372819];
        app.run("tools.setColors", json!({"foreground": original, "background": original})).unwrap();
        let mut h = harness(app, false);
        h.get_by_label(label).click();
        h.run_steps(3);
        h.get_by_label("Color Picker").click();
        h.run_steps(3);
        let id = h.state().ui.dialogs.last().expect("the Color Picker dialog").id;
        let opened = h.state().ui.dialogs.last().unwrap().fields["color"].clone();
        focus_hex(&mut h);
        for ch in "12AB".chars() {
            h.event(Event::Text(ch.to_string()));
            h.run_steps(1);
        }
        assert_eq!(h.state().ui.dialogs.last().unwrap().fields["color"], opened, "four digits are not a colour yet");
        for ch in "EF".chars() {
            h.event(Event::Text(ch.to_string()));
            h.run_steps(1);
        }
        assert_eq!(h.state().ui.dialogs.last().unwrap().fields["color"], "#12abef", "the partial text was kept");
        // An invalid/multi-byte value leaves the colour untouched.
        h.key_press_modifiers(Modifiers::COMMAND, Key::A);
        h.run_steps(1);
        h.event(Event::Text("颜色".into()));
        h.run_steps(2);
        assert_eq!(h.state().ui.dialogs.last().unwrap().fields["color"], "#12abef");
        crate::dialogs::confirm(h.state_mut(), id).unwrap();
        let want = [18.0 / 255.0, 171.0 / 255.0, 239.0 / 255.0, original[3]];
        let tools = &h.state().session.tools;
        assert_eq!(if label == "Fill color" { tools.foreground } else { tools.background }, want);
        assert_eq!(if label == "Fill color" { tools.background } else { tools.foreground }, original);
        h.run_steps(2);
        finish_shape(h.state_mut(), Tool::Rectangle, [20.0, 20.0], [100.0, 80.0], Modifiers::NONE);
        let drawn = shape(h.state());
        let paint = if label == "Fill color" { drawn.fill.as_ref().and_then(color_of) } else { drawn.stroke.as_ref().and_then(|s| color_of(&s.paint)) };
        assert_eq!(paint, Some(want), "the next shape takes the typed colour");
    }
}

#[test]
fn a_pasted_hex_colour_updates_the_selected_shape_and_undoes_once() {
    for label in ["Fill color", "Stroke color"] {
        let mut app = app("rgb", 32);
        app.run("tools.setColors", json!({"foreground": [0.2, 0.3, 0.4, 0.6], "background": [0.5, 0.6, 0.7, 0.8]})).unwrap();
        app.ui.tool_options.stroke_width = 3.0;
        finish_shape(&mut app, Tool::Rectangle, [20.0, 20.0], [100.0, 80.0], Modifiers::NONE);
        let original = shape(&app).clone();
        let history = app.session.active().unwrap().history.past_len();
        let mut h = harness(app, false);
        h.get_by_label(label).click();
        h.run_steps(3);
        h.get_by_label("Color Picker").click();
        h.run_steps(3);
        let id = h.state().ui.dialogs.last().expect("the Color Picker dialog").id;
        focus_hex(&mut h);
        h.event(Event::Paste("#12ABEF".into()));
        h.run_steps(2);
        crate::dialogs::confirm(h.state_mut(), id).unwrap();
        h.run_steps(2);
        let edited = shape(h.state());
        let (paint, untouched) = if label == "Fill color" {
            (edited.fill.as_ref().and_then(color_of), edited.stroke == original.stroke)
        } else {
            (edited.stroke.as_ref().and_then(|s| color_of(&s.paint)), edited.fill == original.fill)
        };
        let alpha = if label == "Fill color" { 0.6 } else { 0.8 };
        let want = [18.0 / 255.0, 171.0 / 255.0, 239.0 / 255.0, alpha];
        assert_eq!(paint, Some(want));
        assert!(untouched, "the other paint stays unchanged");
        assert_eq!(h.state().session.active().unwrap().history.past_len(), history + 1);
        h.state_mut().run("edit.undo", json!({})).unwrap();
        assert_eq!(shape(h.state()), &original);
    }
}

#[test]
fn picking_tool_colours_enables_paint_and_reaches_the_next_shape() {
    let mut app = app("rgb", 32);
    app.ui.tool_options.shape_fill = false;
    let mut h = harness(app, false);
    for (label, swatch) in [("Fill color", "RGB Red"), ("Stroke color", "RGB Blue")] {
        h.get_by_label(label).click();
        h.run_steps(3);
        click_swatch(&mut h, "RGB", swatch);
        h.key_press(Key::Escape);
        h.run_steps(2);
    }
    assert!(h.state().ui.tool_options.shape_fill);
    assert_eq!(h.state().ui.tool_options.stroke_width, 3.0);
    assert_eq!(h.state().session.tools.foreground, [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(h.state().session.tools.background, [0.0, 0.0, 1.0, 1.0]);
    // Both join the Recently Used Colors, newest first.
    let recent: Vec<String> = crate::paint_picker::recent_colors(&h.state().session).into_iter().map(crate::color_picker_ui::hex).collect();
    assert_eq!(recent, ["#0000ff", "#ff0000"]);
    finish_shape(h.state_mut(), Tool::Rectangle, [20.0, 20.0], [100.0, 80.0], Modifiers::NONE);
    assert_eq!(shape(h.state()).fill, Some(solid_paint([1.0, 0.0, 0.0, 1.0])));
    assert_eq!(shape(h.state()).stroke.as_ref().unwrap().paint, solid_paint([0.0, 0.0, 1.0, 1.0]));
}

/// The popover is Photoshop's: the four paint types along the top with the Color Picker button at
/// the right, then Recently Used Colors over the swatch folders. Without a selected shape, the
/// next shape's defaults hold colours only, so Gradient is off; Pattern is off until shapes
/// render pattern paint.
#[test]
fn popover_lays_out_photoshops_paint_types_recent_colours_and_folders() {
    let mut app = app("rgb", 8);
    crate::paint_picker::remember(&mut app, [0.2, 0.4, 0.6]);
    let mut h = harness(app, false);
    h.get_by_label("Fill color").click();
    h.run_steps(3);
    let types: Vec<Rect> = ["No Color", "Solid Color", "Gradient", "Pattern"].iter().map(|l| h.get_by_label(l).rect()).collect();
    for w in types.windows(2) {
        assert!((w[0].top() - w[1].top()).abs() < 0.5 && w[0].right() <= w[1].left(), "one row, Photoshop's order");
    }
    let picker = h.get_by_label("Color Picker").rect();
    assert!((picker.center().y - types[0].center().y).abs() < 2.0 && picker.left() > types[3].right() + 40.0, "Color Picker at the top right");
    assert!(h.get_by_label("Gradient").accesskit_node().is_disabled() && h.get_by_label("Pattern").accesskit_node().is_disabled());
    assert!(!h.get_by_label("Solid Color").accesskit_node().is_disabled());
    let recent = h.get_by_label("Recently Used Colors").rect();
    let swatch = h.get_by_label("#336699").rect();
    assert!(recent.top() > types[0].bottom() && swatch.top() > recent.bottom());
    let folders: Vec<Rect> = ["Grays", "RGB", "Pastel", "Pure", "Dark"].iter().map(|l| h.get_by_label(l).rect()).collect();
    assert!(folders[0].top() > swatch.bottom() && folders.windows(2).all(|w| w[1].top() > w[0].top()), "folders below, in library order");
    assert!(h.query_by_label("RGB Red").is_none(), "folders start closed");
    // The egui picker's channel fields are gone.
    assert!(h.query_by_label("U8").is_none());
}

#[test]
fn colour_picker_button_opens_the_full_picker_on_the_paint() {
    let mut app = app("rgb", 8);
    app.run("tools.setColors", json!({"foreground": "#102030", "background": "#405060"})).unwrap();
    let mut h = harness(app, false);
    h.get_by_label("Stroke color").click();
    h.run_steps(3);
    h.get_by_label("Color Picker").click();
    h.run_steps(3);
    let d = h.state().ui.dialogs.last().expect("the Color Picker dialog");
    assert_eq!(d.fields["__label"], "Color Picker (Stroke Color)");
    assert_eq!(d.fields["color"], "#405060", "on the next shape's stroke colour");
    assert!(!egui::Popup::is_any_open(&h.ctx), "the popover gives way to the dialog");
    let id = d.id;
    h.state_mut().ui.dialog_mut(id).unwrap().fields.insert("color".into(), json!("#ff8800"));
    crate::dialogs::confirm(h.state_mut(), id).unwrap();
    let app = h.state();
    assert_eq!(crate::color_picker_ui::hex([app.session.tools.background[0], app.session.tools.background[1], app.session.tools.background[2]]), "#ff8800");
    assert_eq!(app.ui.tool_options.stroke_width, 3.0, "picking a stroke colour turns the stroke on");
    assert_eq!(app.session.active().unwrap().history.past_len(), 0, "tool defaults do not edit the document");
    assert_eq!(crate::paint_picker::recent_colors(&app.session).first().copied().map(crate::color_picker_ui::hex).as_deref(), Some("#ff8800"));
}

#[test]
fn no_colour_and_solid_choices_are_explicit_live_tool_defaults() {
    let mut h = harness(app("rgb", 8), false);
    h.get_by_label("Fill color").click();
    h.run_steps(3);
    h.get_by_label("No Color").click();
    h.run_steps(2);
    assert!(!h.state().ui.tool_options.shape_fill);
    h.get_by_label("Solid Color").click();
    h.run_steps(2);
    assert!(h.state().ui.tool_options.shape_fill);
    h.key_press(Key::Escape);
    h.run_steps(2);
    h.get_by_label("Stroke color").click();
    h.run_steps(3);
    h.get_by_label("Solid Color").click();
    h.run_steps(2);
    assert_eq!(h.state().ui.tool_options.stroke_width, 3.0, "choosing unchanged white enables a stroke");
    h.get_by_label("No Color").click();
    h.run_steps(2);
    assert_eq!(h.state().ui.tool_options.stroke_width, 0.0);
    assert_eq!(h.state().session.active().unwrap().history.past_len(), 0, "tool defaults do not edit the document");
}

#[test]
fn float_paints_match_preview_creation_native_roundtrip_and_document_depth() {
    let foreground = [0.123456, 0.345678, 0.567891, 0.372819];
    let background = [0.891234, 0.234567, 0.456789, 0.618237];
    for (mode, depth) in [("rgb", 8), ("rgb", 16), ("rgb", 32), ("gray", 16), ("cmyk", 8), ("lab", 16)] {
        for tool in [Tool::Rectangle, Tool::Triangle, Tool::EllipseShape] {
            let mut app = app(mode, depth);
            set_tool_paint(&mut app, false, PaintChange::Color(foreground)).unwrap();
            set_tool_paint(&mut app, true, PaintChange::Color(background)).unwrap();
            app.ui.tool_options.shape_stroke.cap = photocraft_doc::LineCap::Round;
            app.ui.tool_options.shape_stroke.dashes = vec![0.0, 2.0];
            let preview = shape_preview(&app, tool, [20.0, 20.0], [100.0, 80.0], Modifiers::NONE).unwrap();
            finish_shape(&mut app, tool, [20.0, 20.0], [100.0, 80.0], Modifiers::NONE);
            let committed = shape(&app).clone();
            assert_eq!(preview.fill, committed.fill);
            assert_eq!(preview.stroke, committed.stroke);
            assert_eq!(preview.path, committed.path);
            assert_eq!(committed.fill, Some(solid_paint(foreground)));
            assert_eq!(committed.stroke.as_ref().unwrap().paint, solid_paint(background));
            let doc = &app.session.active().unwrap().doc;
            let rendered = photocraft_vector::render_shape(&preview, doc.pixel_format(), doc.bounds());
            assert_eq!(committed.cache.as_ref().unwrap(), &rendered, "{tool:?} {mode}/{depth}");
            let bytes = photocraft_format::save_to_bytes(doc, &Default::default()).unwrap();
            let loaded = photocraft_format::load_from_bytes(&bytes).unwrap();
            let id = app.session.active().unwrap().active_layer.unwrap();
            let LayerContent::Shape(saved) = &loaded.layer(id).unwrap().content else { panic!("saved editable shape") };
            assert_eq!((&saved.fill, &saved.stroke), (&committed.fill, &committed.stroke));
            assert_saved_path(&saved.path, &committed.path);
            app.run("edit.undo", json!({})).unwrap();
            app.run("edit.redo", json!({})).unwrap();
            assert_eq!(shape(&app), &committed);
        }
    }
}

#[test]
fn transparent_and_absent_paints_remain_distinct_and_open_pen_keeps_foreground() {
    let mut app = app("rgb", 32);
    let transparent = [0.124578, 0.564321, 0.765432, 0.0];
    set_tool_paint(&mut app, false, PaintChange::Color(transparent)).unwrap();
    set_tool_paint(&mut app, true, PaintChange::Color(transparent)).unwrap();
    let preview = shape_preview(&app, Tool::Rectangle, [20.0, 20.0], [80.0, 70.0], Modifiers::NONE).unwrap();
    assert_eq!(preview.fill, Some(solid_paint(transparent)));
    assert_eq!(preview.stroke.as_ref().unwrap().paint, solid_paint(transparent));
    finish_shape(&mut app, Tool::Rectangle, [20.0, 20.0], [80.0, 70.0], Modifiers::NONE);
    assert_eq!(shape(&app).fill, preview.fill);
    assert_eq!(shape(&app).stroke, preview.stroke);
    assert_eq!(shape(&app).cache.as_ref().unwrap().pixel(40, 40), vec![0.0; 4]);
    set_tool_paint(&mut app, false, PaintChange::None).unwrap();
    set_tool_paint(&mut app, true, PaintChange::None).unwrap();
    finish_shape(&mut app, Tool::Triangle, [20.0, 20.0], [80.0, 70.0], Modifiers::NONE);
    assert!(shape(&app).fill.is_none() && shape(&app).stroke.is_none());
    let foreground = [0.836521, 0.125468, 0.745632, 0.438271];
    app.run("tools.setColors", json!({"foreground": foreground, "background": [0.0, 1.0, 0.0, 1.0]})).unwrap();
    app.ui.tool_options.vector_mode = "shape".into();
    app.ui.pen = Some(PenPath { knots: vec![[[20.0, 20.0]; 3], [[80.0, 60.0]; 3]], ..Default::default() });
    pen_commit(&mut app, false);
    assert!(shape(&app).fill.is_none());
    assert!(!shape(&app).path.subpaths[0].closed);
    assert_eq!(shape(&app).stroke.as_ref().unwrap().paint, solid_paint(foreground));
}

#[test]
fn selected_shape_style_is_available_when_absent_and_dismissal_does_not_enable_it() {
    let mut app = app("rgb", 8);
    app.run("shape.create", json!({"rect": [20,20,80,60], "stroke": null})).unwrap();
    let doc = app.session.active().unwrap().doc.clone();
    let history = app.session.active().unwrap().history.past_len();
    let mut h = harness(app, true);
    h.get_by_label("Stroke Options").click();
    h.run_steps(3);
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert_eq!(h.state().session.active().unwrap().doc, doc);
    h.get_by_label("Stroke Options").click();
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::RadioButton, "Dotted").click();
    h.run_steps(3);
    assert_eq!(shape(h.state()).stroke.as_ref().unwrap().width, 3.0);
    assert_eq!(shape(h.state()).stroke.as_ref().unwrap().dashes, [0.0, 2.0]);
    assert_eq!(h.state().session.active().unwrap().history.past_len(), history + 1);
    h.key_press(Key::Escape);
    h.run_steps(2);
    h.state_mut().run("edit.undo", json!({})).unwrap();
    assert!(shape(h.state()).stroke.is_none());
    h.state_mut().run("edit.redo", json!({})).unwrap();
    assert_eq!(shape(h.state()).stroke.as_ref().unwrap().dashes, [0.0, 2.0]);
}

#[test]
fn selected_shape_swatch_clicks_are_one_undo_step_each_and_keep_tool_defaults() {
    let mut app = app("rgb", 32);
    app.run("shape.create", json!({"rect": [20,20,80,60], "fill": [0.2, 0.4, 0.6, 0.5], "stroke": {"width": 5, "color": "#ee3355"}})).unwrap();
    let original = shape(&app).clone();
    let defaults = (app.session.tools.foreground, app.session.tools.background, app.ui.tool_options.clone());
    let history = app.session.active().unwrap().history.past_len();
    let mut h = harness(app, true);
    h.get_by_label("Set shape fill type").click();
    h.run_steps(4);
    assert_eq!(h.state().session.active().unwrap().history.past_len(), history, "opening changes nothing");
    click_swatch(&mut h, "RGB", "RGB Green");
    assert_eq!(shape(h.state()).fill, Some(solid_paint([0.0, 1.0, 0.0, 0.5])), "the fill's alpha stays");
    click_swatch(&mut h, "RGB", "RGB Blue");
    assert_eq!(h.state().session.active().unwrap().history.past_len(), history + 2);
    assert_eq!(shape(h.state()).stroke, original.stroke);
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert_eq!((h.state().session.tools.foreground, h.state().session.tools.background, h.state().ui.tool_options.clone()), defaults);
    h.state_mut().run("edit.undo", json!({})).unwrap();
    assert_eq!(shape(h.state()).fill, Some(solid_paint([0.0, 1.0, 0.0, 0.5])));
    h.state_mut().run("edit.undo", json!({})).unwrap();
    assert_eq!(shape(h.state()).fill, original.fill);
}

#[test]
fn selected_shape_gradient_and_full_picker_edit_the_layer() {
    let mut app = app("rgb", 8);
    app.run("shape.create", json!({"rect": [20,20,80,60], "fill": "#3270c0", "stroke": null})).unwrap();
    let history = app.session.active().unwrap().history.past_len();
    let mut h = harness(app, true);
    h.get_by_label("Set shape fill type").click();
    h.run_steps(3);
    h.get_by_label("Gradient").click();
    h.run_steps(2);
    let group = h.state().session.presets.gradients[0].name.clone();
    let preset = h.state().session.presets.gradients[0].items[0].name.clone();
    click_swatch(&mut h, &group, &preset);
    assert!(matches!(shape(h.state()).fill, Some(photocraft_doc::Fill::Gradient { .. })), "a gradient preset fills the shape");
    assert_eq!(h.state().session.active().unwrap().history.past_len(), history + 1);
    h.key_press(Key::Escape);
    h.run_steps(2);
    // The stroke is off: the Color Picker turns it on in the picked colour, as one edit.
    h.get_by_label("Set shape stroke type").click();
    h.run_steps(3);
    h.get_by_label("Color Picker").click();
    h.run_steps(3);
    let d = h.state().ui.dialogs.last().expect("the Color Picker dialog");
    assert_eq!(d.fields["__label"], "Color Picker (Stroke Color)");
    let id = d.id;
    h.state_mut().ui.dialog_mut(id).unwrap().fields.insert("color".into(), json!("#00ff00"));
    crate::dialogs::confirm(h.state_mut(), id).unwrap();
    let stroke = shape(h.state()).stroke.clone().expect("the stroke is on");
    assert_eq!((stroke.paint, stroke.width), (solid_paint([0.0, 1.0, 0.0, 1.0]), 3.0));
    assert_eq!(h.state().session.active().unwrap().history.past_len(), history + 2);
    // The fill's picker from the gradient: OK on the colour it opened with still makes it solid.
    h.get_by_label("Set shape fill type").click();
    h.run_steps(3);
    h.get_by_label("Color Picker").click();
    h.run_steps(3);
    let id = h.state().ui.dialogs.last().unwrap().id;
    crate::dialogs::confirm(h.state_mut(), id).unwrap();
    assert_eq!(shape(h.state()).fill, Some(solid_paint(h.state().session.tools.foreground)));
}

/// #3997, #3999: after drawing a shape, the options bar's Fill swatch recolours it, as Stroke
/// does (in v0.7.0 the Fill swatch was only a preview of the foreground colour).
#[test]
fn options_bar_fill_swatch_recolours_the_shape_just_drawn() {
    let mut app = app("rgb", 8);
    app.run("tools.setColors", json!({"foreground": "#ff0000", "background": "#0000ff"})).unwrap();
    app.ui.tool_options.stroke_width = 3.0;
    finish_shape(&mut app, Tool::Rectangle, [20.0, 20.0], [100.0, 80.0], Modifiers::NONE);
    let original = shape(&app).clone();
    let history = app.session.active().unwrap().history.past_len();
    crate::paint_picker::remember(&mut app, [0.2, 0.4, 0.6]);
    let mut h = harness(app, false);
    h.get_by_label("Fill color").click();
    h.run_steps(4);
    h.get_by_label("#336699").click();
    h.run_steps(2);
    let edited = shape(h.state()).clone();
    assert_ne!(edited.fill, original.fill, "the Fill swatch recoloured the shape");
    assert_eq!(edited.fill.as_ref().and_then(color_of), Some(h.state().session.tools.foreground), "and the next shape's fill");
    assert_eq!(edited.stroke, original.stroke, "the stroke is untouched");
    assert_eq!(h.state().session.active().unwrap().history.past_len(), history + 1);
    h.state_mut().run("edit.undo", json!({})).unwrap();
    assert_eq!(shape(h.state()).fill, original.fill);
}
