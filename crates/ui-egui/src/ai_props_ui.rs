//! Properties panel › AI Retouching (MVP-2 of `docs/requirements-ai-inline-editing.md`):
//! the local backend's status and the parameters of the two AI retouching commands.
//!
//! The Apply buttons send the same engine commands the menu dialogs do
//! (`ai.skinEnhance`, `ai.removeObject`), which run as background jobs with
//! progress and Esc cancel — one behaviour for the panel, the dialog, the CLI
//! and MCP.

use egui::{RichText, TextEdit};
use serde_json::json;

use crate::theme::Tokens;
use crate::{PhotocraftApp, widgets};
/// The AI Retouching section on a pixel layer's Properties (both themes).
pub fn section(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    ui.label(RichText::new(tl!("AI Retouching")).font(crate::theme::semibold(12.5)).color(t.text));
    ui.add_space(4.0);
    backend_row(app, ui);
    ui.add_space(6.0);
    widgets::hairline(ui);
    ui.add_space(6.0);
    skin_enhance(app, ui);
    ui.add_space(6.0);
    widgets::hairline(ui);
    ui.add_space(6.0);
    remove_object(app, ui);
}

/// The configured local backend (Preferences › AI) and a connection test.
fn backend_row(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let ai = &app.session.prefs().ai;
    ui.label(RichText::new(format!("{} · {}", ai.local_model, ai.local_url)).color(t.text_dim).size(11.5).truncate());
    if widgets::secondary_button(ui, tl!("Test Connection"), 110.0).clicked() {
        match app.run("ai.pingBackend", json!({})) {
            Ok(v) => {
                let ok = v.get("ok").and_then(serde_json::Value::as_bool).unwrap_or(false);
                app.ui.status = if ok {
                    tl!("AI backend is reachable").into()
                } else {
                    format!("{}: {}", tl!("AI backend unreachable"), v.get("reason").and_then(serde_json::Value::as_str).unwrap_or_default())
                };
                app.ui.status_error = !ok;
            }
            Err(e) => {
                app.ui.status = e;
                app.ui.status_error = true;
            }
        }
    }
}

/// Skin Enhance's parameters and its Apply button.
fn skin_enhance(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    ui.label(RichText::new(tl!("Skin Enhance")).color(crate::theme::Tokens::get(ui.ctx()).text_dim));
    ui.add_space(2.0);
    let (mut strength, mut radius, mut preserve) = {
        let ai = &app.ui.ai;
        (ai.skin_strength, ai.skin_radius, ai.skin_preserve_detail)
    };
    widgets::slider_row(ui, tl!("Strength"), &mut strength, 0.0..=100.0, "%", None);
    widgets::slider_row(ui, tl!("Radius"), &mut radius, 2.0..=20.0, "px", None);
    widgets::checkbox(ui, &mut preserve, tl!("Preserve Detail"));
    if widgets::primary_button(ui, tl!("Apply Skin Enhance"), ui.available_width()).clicked() {
        {
            let ai = &mut app.ui.ai;
            (ai.skin_strength, ai.skin_radius, ai.skin_preserve_detail) = (strength, radius, preserve);
        }
        run(app, "ai.skinEnhance", json!({"strength": strength, "smoothRadius": radius, "preserveDetail": preserve}));
    }
}

/// Remove Object's parameters and its Apply button (needs a selection).
fn remove_object(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    ui.label(RichText::new(tl!("Remove Object")).color(t.text_dim));
    ui.add_space(2.0);
    let has_selection = app
        .session
        .active()
        .is_some_and(|st| st.doc.selection.as_ref().is_some_and(|sel| !sel.content_bounds().intersect(&st.doc.bounds()).is_empty()));
    let (mut strength, mut new_layer) = { let ai = &app.ui.ai; (ai.remove_strength, ai.remove_new_layer) };
    let mut prompt = app.ui.ai.remove_prompt.clone();
    ui.add_enabled_ui(has_selection, |ui| {
        ui.add(TextEdit::singleline(&mut prompt).hint_text(tl!("Fill hint for the model (optional)")).desired_width(ui.available_width()));
        widgets::slider_row(ui, tl!("Strength"), &mut strength, 0.0..=100.0, "%", None);
        widgets::checkbox(ui, &mut new_layer, tl!("Create a new layer"));
        if widgets::primary_button(ui, tl!("Remove Object"), ui.available_width()).clicked() {
            {
                let ai = &mut app.ui.ai;
                ai.remove_prompt = prompt.clone();
                (ai.remove_strength, ai.remove_new_layer) = (strength, new_layer);
            }
            let params = json!({
                "prompt": if prompt.is_empty() { None::<String> } else { Some(prompt.as_str()) },
                "strength": strength / 100.0,
                "output": if new_layer { "newLayer" } else { "inPlace" },
            });
            run(app, "ai.removeObject", params);
        }
    });
    if !has_selection {
        ui.label(RichText::new(tl!("Make a selection to remove an object")).color(t.text_faint).size(11.0));
    }
}

/// Run an AI command (a background job on the desktop app) and report failures.
fn run(app: &mut PhotocraftApp, id: &str, params: serde_json::Value) {
    if let Err(e) = app.run(id, params) {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_section_draws_on_a_pixel_layer_and_defaults_are_sane() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 32, "height": 32})).unwrap();
        assert_eq!(app.ui.ai.skin_strength, 50.0);
        assert_eq!(app.ui.ai.remove_strength, 85.0);
        assert!(app.ui.ai.remove_new_layer);
        egui::Context::default().run_ui(Default::default(), |ui| section(&mut app, ui)).textures_delta.clear();
        // Remove Object is disabled without a selection, with a hint instead.
        assert!(app.ui.ai.remove_prompt.is_empty());
    }

    #[test]
    fn the_ai_state_round_trips_through_serde() {
        let state = crate::state::AiRetouchState { remove_prompt: "clear".into(), ..Default::default() };
        let v = serde_json::to_value(&state).unwrap();
        assert_eq!(v["remove_prompt"], json!("clear"));
        let back: crate::state::AiRetouchState = serde_json::from_value(v).unwrap();
        assert_eq!(back, state);
        // Older settings without the field fall back to the defaults.
        let back: crate::state::AiRetouchState = serde_json::from_value(json!({})).unwrap();
        assert_eq!(back, crate::state::AiRetouchState::default());
    }
}
