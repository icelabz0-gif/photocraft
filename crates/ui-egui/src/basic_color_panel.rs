//! Window › Basic Color: the floating panel around a `BasicColor` adjustment layer.
//!
//! The panel is the Lightroom-style local flow on top of the existing engine commands: it creates
//! the adjustment layer (`layer.newAdjustmentLayer.basicColor`), edits it through the shared
//! Properties editor ([`crate::adjust_editors::layer_editor`]), and prepares the Brush so painting
//! on the layer mask reveals the effect only where the user paints.
//!
//! All state lives in [`BasicColorState`], part of [`crate::state::UiState`]. The panel never
//! touches the document directly: every action is an engine command, queued while the panel draws
//! and applied afterwards so no UI-state borrow is held across a command.

use egui::{Align2, CornerRadius, Sense, Stroke, vec2};
use photocraft_doc::{Adjustment, LayerContent, LayerId};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::theme::{self, Tokens};
use crate::{PhotocraftApp, icons, widgets};

/// Panel width, in points, like Brush Settings' and Retouch AI's fixed cards.
pub const WIDTH: f32 = 352.0;

/// The panel's English source keys, as data rather than literal `tl!` calls.
///
/// Every key below has a `vi.tsv` row (checked by `tests::vietnamese_covers_every_panel_key`);
/// the other complete catalogs fall back to English for this panel until a translator covers it.
/// The editor reuses `WHITE_BALANCE`, `TONE` and `PRESENCE` for its section labels.
pub(crate) mod text {
    pub const TITLE: &str = "Basic Color";
    pub const CLOSE: &str = "Close";
    pub const OPEN_IMAGE_TO_START: &str = "Open an image to start";
    pub const OTHER_LAYER: &str = "The active layer isn't a Basic Color layer. Create one above it, then paint the region to adjust.";
    pub const CREATE: &str = "Create Basic Color layer";
    pub const RESET: &str = "Reset";
    pub const MASK: &str = "Mask";
    pub const NO_PIXEL_MASK: &str = "No pixel mask yet. Apply to whole image adds one; then paint to narrow it.";
    pub const PAINT_NEW_REGION: &str = "Paint a new region";
    pub const ADD_TO_REGION: &str = "Add to region";
    pub const APPLY_WHOLE: &str = "Apply to whole image";
    pub const SHOW_MASK: &str = "Show mask";
    pub const HIDE_MASK: &str = "Hide mask";
    pub const HINT: &str = "Paint white to reveal, black to hide. Sliders affect only the painted region.";

    // Editor section labels (shared with `adjust_editors`).
    pub const WHITE_BALANCE: &str = "White Balance";
    pub const TONE: &str = "Tone";
    pub const PRESENCE: &str = "Presence";

    // Command errors.
    pub const CREATE_FAILED: &str = "Create Basic Color layer failed: {error}";
    pub const RESET_FAILED: &str = "Reset failed: {error}";
    pub const MASK_FAILED: &str = "Mask change failed: {error}";

    /// Every fixed key above, for `tests::vietnamese_covers_every_panel_key`.
    #[cfg(test)]
    pub const ALL: &[&str] = &[
        TITLE,
        CLOSE,
        OPEN_IMAGE_TO_START,
        OTHER_LAYER,
        CREATE,
        RESET,
        MASK,
        NO_PIXEL_MASK,
        PAINT_NEW_REGION,
        ADD_TO_REGION,
        APPLY_WHOLE,
        SHOW_MASK,
        HIDE_MASK,
        HINT,
        WHITE_BALANCE,
        TONE,
        PRESENCE,
        CREATE_FAILED,
        RESET_FAILED,
        MASK_FAILED,
    ];
}

/// Basic Color panel state, persisted in [`crate::state::UiState`] so it survives a relaunch.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BasicColorState {
    /// The last command error shown under the controls (empty: none).
    pub status: String,
    pub status_error: bool,
}

/// The active layer when it is a Basic Color adjustment: everything the panel needs, copied out so
/// the engine calls and the shared editor don't hold a borrow of the session.
struct ActiveBasic {
    id: LayerId,
    adjustment: Adjustment,
    name: String,
    has_mask: bool,
    overlay: bool,
}

fn active_basic_color(app: &PhotocraftApp) -> Option<ActiveBasic> {
    let st = app.session.active()?;
    let id = st.active_layer?;
    let layer = st.doc.layer(id)?;
    let LayerContent::Adjustment(adjustment) = &layer.content else { return None };
    if !matches!(adjustment, Adjustment::BasicColor { .. }) {
        return None;
    }
    Some(ActiveBasic {
        id,
        adjustment: adjustment.clone(),
        name: layer.name.clone(),
        has_mask: layer.mask.is_some(),
        overlay: photocraft_engine::mask_view_cmds::current(st)
            .is_some_and(|view| view.layer == id && view.mode == photocraft_engine::mask_view_cmds::MaskViewMode::Overlay),
    })
}

/// What the panel asks the app to do after it has drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Create,
    Reset { layer: LayerId },
    PaintNewRegion { layer: LayerId },
    AddToRegion,
    ApplyWhole { layer: LayerId },
    ToggleMask { layer: LayerId },
}

fn set_status(state: &mut BasicColorState, message: String, error: bool) {
    state.status = message;
    state.status_error = error;
}

fn failed(app: &mut PhotocraftApp, template: &str, error: &str) {
    let message = crate::i18n::fmt(tl!(template), &[("error", error)]);
    set_status(&mut app.ui.basic_color, message, true);
}

/// Select the Brush with the layer mask targeted and white/black foreground/background, so a
/// stroke reveals the adjustment. The colours are set after [`PhotocraftApp::sync_mask_targets`],
/// which brings back the mask's own colour pair and could otherwise undo the choice.
fn paint_setup(app: &mut PhotocraftApp) {
    crate::tool_memory::select(app, crate::state::Tool::Brush);
    app.ui.mask_target = true;
    app.ui.vector_mask_target = false;
    app.sync_mask_targets();
    if let Err(error) = app.run("tools.setColors", json!({"foreground": "#ffffff", "background": "#000000"})) {
        failed(app, text::MASK_FAILED, &error);
    }
}

fn apply(app: &mut PhotocraftApp, action: Action) {
    app.ui.basic_color.status.clear();
    app.ui.basic_color.status_error = false;
    match action {
        Action::Create => {
            if let Err(error) = app.run("layer.newAdjustmentLayer.basicColor", json!({})) {
                failed(app, text::CREATE_FAILED, &error);
            }
        }
        Action::Reset { layer } => {
            // Only the layer id: the engine resets the kind's parameters to their defaults.
            if app.live_adjust.as_ref().is_some_and(|(id, _)| *id == layer) {
                app.live_adjust = None;
            }
            if let Err(error) = app.run("layer.setAdjustment", json!({"layer": layer.0})) {
                failed(app, text::RESET_FAILED, &error);
            }
        }
        Action::PaintNewRegion { layer } => {
            if let Err(error) = app.run("layer.layerMask.hideAll", json!({"layer": layer.0})) {
                failed(app, text::MASK_FAILED, &error);
                return;
            }
            paint_setup(app);
        }
        Action::AddToRegion => paint_setup(app),
        Action::ApplyWhole { layer } => {
            if let Err(error) = app.run("layer.layerMask.revealAll", json!({"layer": layer.0})) {
                failed(app, text::MASK_FAILED, &error);
            }
        }
        Action::ToggleMask { layer } => {
            if let Err(error) = app.run("layer.toggleMaskOverlay", json!({"layer": layer.0})) {
                failed(app, text::MASK_FAILED, &error);
            }
        }
    }
}

fn mask_controls(ui: &mut egui::Ui, info: &ActiveBasic, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    widgets::hairline(ui);
    ui.add_space(2.0);
    ui.label(egui::RichText::new(tl!(text::MASK)).font(theme::semibold(12.0)).color(t.text));
    if !info.has_mask {
        // Vector mask only, or none: painting needs a pixel mask, so the paint controls wait.
        ui.add(egui::Label::new(egui::RichText::new(tl!(text::NO_PIXEL_MASK)).size(11.0).color(t.text_faint)).wrap());
        if widgets::secondary_button(ui, tl!(text::APPLY_WHOLE), 0.0).clicked() {
            actions.push(Action::ApplyWhole { layer: info.id });
        }
        return;
    }
    ui.horizontal(|ui| {
        if widgets::secondary_button(ui, tl!(text::PAINT_NEW_REGION), 0.0).clicked() {
            actions.push(Action::PaintNewRegion { layer: info.id });
        }
        if widgets::secondary_button(ui, tl!(text::ADD_TO_REGION), 0.0).clicked() {
            actions.push(Action::AddToRegion);
        }
    });
    ui.horizontal(|ui| {
        if widgets::secondary_button(ui, tl!(text::APPLY_WHOLE), 0.0).clicked() {
            actions.push(Action::ApplyWhole { layer: info.id });
        }
        let mask_label = if info.overlay { tl!(text::HIDE_MASK) } else { tl!(text::SHOW_MASK) };
        if ui.selectable_label(info.overlay, mask_label).clicked() {
            actions.push(Action::ToggleMask { layer: info.id });
        }
    });
    ui.add(egui::Label::new(egui::RichText::new(tl!(text::HINT)).size(10.5).color(t.text_faint)).wrap());
}

/// Window › Basic Color. Draws nothing when the menu closed it; never needs a document.
pub fn window(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !app.ui.panels.basic_color {
        return;
    }
    let has_document = app.session.active_index().is_some();
    let active = active_basic_color(app);
    let mut open = true;
    let mut actions = Vec::new();
    let t = Tokens::get(ctx);
    let frame = egui::Frame::NONE
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.card_border))
        .corner_radius(CornerRadius::same(t.radius_lg as u8))
        .shadow(egui::Shadow { offset: [0, 10], blur: 30, spread: 0, color: t.shadow })
        .inner_margin(egui::Margin::same(10));
    let canvas = app.last_canvas_rect;
    let max_height = (ctx.content_rect().height() * 0.72).max(320.0);
    egui::Window::new(tl!(text::TITLE))
        .id(egui::Id::new("basic-color"))
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .frame(frame)
        .default_pos(egui::pos2((canvas.right() - WIDTH - 30.0).max(canvas.left() + 8.0), canvas.top() + 24.0))
        .show(ctx, |ui| {
            ui.set_width(WIDTH);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tl!(text::TITLE)).font(theme::semibold(13.0)).color(t.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::button(ui, "x", 20.0, false, tl!(text::CLOSE)).clicked() {
                        open = false;
                    }
                });
            });
            ui.add_space(5.0);
            widgets::hairline(ui);
            ui.add_space(5.0);
            if !has_document {
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
                ui.painter().rect_filled(rect, t.radius_sm, t.field);
                ui.painter().text(
                    egui::pos2(rect.left() + 8.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    tl!(text::OPEN_IMAGE_TO_START),
                    theme::medium(11.5),
                    t.warning,
                );
                // Painted text has no accessibility node of its own; name the banner for tests,
                // screen readers and the control channel.
                let hint = ui.interact(rect, ui.id().with("basic-color-no-document"), Sense::hover());
                hint.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, tl!(text::OPEN_IMAGE_TO_START)));
                return;
            }
            egui::ScrollArea::vertical().id_salt("basic-color-body").max_height(max_height).auto_shrink([false, true]).show(ui, |ui| {
                ui.set_width(ui.available_width());
                match &active {
                    Some(info) => {
                        ui.horizontal(|ui| {
                            ui.add(egui::Label::new(egui::RichText::new(&info.name).size(12.0).color(t.text)).truncate());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if widgets::secondary_button(ui, tl!(text::RESET), 0.0).clicked() {
                                    actions.push(Action::Reset { layer: info.id });
                                }
                            });
                        });
                        mask_controls(ui, info, &mut actions);
                        ui.add_space(4.0);
                        crate::adjust_editors::layer_editor(app, ui, info.id, &info.adjustment);
                    }
                    None => {
                        ui.add(egui::Label::new(egui::RichText::new(tl!(text::OTHER_LAYER)).size(11.5).color(t.text_dim)).wrap());
                        ui.add_space(4.0);
                        if widgets::primary_button(ui, tl!(text::CREATE), 0.0).clicked() {
                            actions.push(Action::Create);
                        }
                    }
                }
                if !app.ui.basic_color.status.is_empty() {
                    let color = if app.ui.basic_color.status_error { t.danger } else { t.text_dim };
                    ui.add(egui::Label::new(egui::RichText::new(&app.ui.basic_color.status).size(11.0).color(color)).wrap());
                }
                ui.add_space(2.0);
            });
        });
    if !open {
        app.ui.panels.basic_color = false;
    }
    for action in actions {
        apply(app, action);
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    fn app() -> PhotocraftApp {
        PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default())
    }

    #[test]
    fn default_state_round_trips_through_serde() {
        let state = BasicColorState::default();
        assert!(state.status.is_empty() && !state.status_error);
        let json = serde_json::to_string(&state).expect("serialize");
        let back: BasicColorState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(state, back);
        // An older layout without the field gets the whole default state.
        let fresh: BasicColorState = serde_json::from_str("{}").expect("missing fields default");
        assert_eq!(fresh, BasicColorState::default());
    }

    #[test]
    fn menu_command_toggles_the_panel() {
        let mut app = app();
        let ctx = egui::Context::default();
        assert!(!app.ui.panels.basic_color);
        crate::menus::invoke(&mut app, &ctx, "window.toggle.basicColor", json!({})).expect("toggle");
        assert!(app.ui.panels.basic_color);
        crate::menus::invoke(&mut app, &ctx, "window.panel.basicColor", json!({})).expect("panel alias");
        assert!(!app.ui.panels.basic_color);
        crate::menus::invoke(&mut app, &ctx, "window.panel.basicColor", json!({})).expect("panel alias");
        assert!(app.ui.panels.basic_color);
    }

    #[test]
    fn menu_catalog_uses_the_toggle_command_and_the_alias_works_in_a_custom_workspace() {
        let entry = crate::menu_catalog::CATALOG
            .iter()
            .find(|(path, label, _, _)| *label == "Basic Color" && path.contains(&"Window"))
            .expect("Window › Basic Color is in the catalog");
        assert_eq!(entry.3, "window.toggle.basicColor");
        let layer = crate::menu_catalog::CATALOG
            .iter()
            .find(|(path, label, _, _)| *label == "Basic Color…" && path.contains(&"New Adjustment Layer"))
            .expect("Layer › New Adjustment Layer › Basic Color… is in the catalog");
        assert_eq!(layer.3, "layer.newAdjustmentLayer.basicColor");

        // A saved custom dock layout makes panel_docking answer for `window.panel.*`, but the
        // Basic Color panel is not dockable: the alias must reach the real toggle command.
        let mut app = app();
        app.run("window.panel.move", json!({"panel":"swatches","anchor":"layers"})).expect("custom layout");
        assert!(app.ui.docking.is_some());
        let ctx = egui::Context::default();
        assert!(!app.ui.panels.basic_color);
        crate::menus::invoke(&mut app, &ctx, "window.panel.basicColor", json!({})).expect("panel alias");
        assert!(app.ui.panels.basic_color);
        let item = crate::menus::menu_items(&app)
            .into_iter()
            .find(|item| item.label == "Basic Color" && item.path.iter().any(|p| p == "Window"))
            .expect("Window › Basic Color is in the menu");
        assert_eq!(item.checked, Some(true), "the menu shows Basic Color checked");
        crate::menus::invoke(&mut app, &ctx, "window.panel.basicColor", json!({})).expect("panel alias");
        assert!(!app.ui.panels.basic_color);
    }

    #[test]
    fn panel_draws_without_a_document() {
        let mut app = app();
        app.ui.panels.basic_color = true;
        let mut h = Harness::builder().with_size(vec2(900.0, 800.0)).build_ui_state(
            |ui, app: &mut PhotocraftApp| {
                let ctx = ui.ctx().clone();
                if !ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                    return;
                }
                window(app, &ctx);
            },
            app,
        );
        PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
        h.run_steps(4);
        assert!(h.state().ui.panels.basic_color);
        assert!(h.query_all_by_label("Open an image to start").next().is_some(), "the no-document hint is shown");
    }

    #[test]
    fn mask_actions_prepare_the_brush_and_apply_to_the_whole_image() {
        let mut session = photocraft_engine::Session::new();
        session.execute("file.new", json!({"width": 64, "height": 64, "mode": "rgb", "depth": 8})).expect("new document");
        session.execute("layer.newAdjustmentLayer.basicColor", json!({})).expect("basic color layer");
        let mut app = PhotocraftApp::new(session, crate::Services::default());
        let id = app.session.active().and_then(|st| st.active_layer).expect("active layer");

        app.run("layer.layerMask.delete", json!({"layer": id.0})).expect("no mask yet");
        apply(&mut app, Action::ApplyWhole { layer: id });
        assert!(
            app.session.active().and_then(|st| st.doc.layer(id)).is_some_and(|l| l.mask.is_some()),
            "Apply to whole image adds the pixel mask painting needs"
        );

        apply(&mut app, Action::PaintNewRegion { layer: id });
        assert_eq!(app.ui.tool, crate::state::Tool::Brush);
        assert!(app.ui.mask_target && !app.ui.vector_mask_target);
        assert_eq!(app.session.tools.foreground, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(app.session.tools.background, [0.0, 0.0, 0.0, 1.0]);
        let st = app.session.active().expect("document");
        let mask = st.doc.layer(id).and_then(|l| l.mask.as_ref()).expect("mask");
        assert_eq!(mask.surface.sample_channel(32, 32, 0), 0.0, "a new region starts hidden");

        apply(&mut app, Action::ToggleMask { layer: id });
        let st = app.session.active().expect("document");
        assert_eq!(
            photocraft_engine::mask_view_cmds::current(st).map(|view| view.mode),
            Some(photocraft_engine::mask_view_cmds::MaskViewMode::Overlay),
            "Show mask follows the overlay view"
        );
    }

    #[test]
    fn editor_params_round_trip_and_default_to_zero() {
        let defaults = photocraft_engine::adjust_params::from_params("basicColor", &json!({}), None, photocraft_doc::ColorMode::Rgb).expect("defaults");
        let params = photocraft_engine::adjust_params::to_params(&defaults);
        for key in ["temperature", "tint", "exposure", "contrast", "highlights", "shadows", "whites", "blacks", "vibrance", "saturation"] {
            assert_eq!(params.get(key).and_then(serde_json::Value::as_f64), Some(0.0), "{key} defaults to 0");
        }
        let changed = json!({"temperature": 40, "tint": -20, "exposure": 0.5, "contrast": 15, "highlights": -30, "shadows": 25, "whites": 10, "blacks": -5, "vibrance": 30, "saturation": -10});
        let adj = photocraft_engine::adjust_params::from_params("basicColor", &changed, None, photocraft_doc::ColorMode::Rgb).expect("values");
        let back = photocraft_engine::adjust_params::to_params(&adj);
        for (key, value) in changed.as_object().into_iter().flatten() {
            let want = value.as_f64();
            assert_eq!(back.get(key.as_str()).and_then(serde_json::Value::as_f64), want, "{key} round-trips");
        }
    }

    /// The panel's data keys skip the literal scanner (the other complete catalogs fall back to
    /// English), so this test is what guarantees the Vietnamese catalog covers every new string.
    #[test]
    fn vietnamese_covers_every_panel_key() {
        let vi = crate::i18n::Lang::from_code("vi").expect("vi is registered");
        let missing: Vec<_> = text::ALL.iter().copied().filter(|key| !crate::i18n::has(vi, key)).collect();
        assert!(missing.is_empty(), "vi: missing Basic Color panel strings: {missing:#?}");
        // The data keys reach `tl!` correctly (not only the catalog).
        let (title, create, white_balance) = crate::i18n::with_language(vi, || (tl!(text::TITLE), tl!(text::CREATE), tl!(text::WHITE_BALANCE)));
        assert_eq!(title, "Chỉnh màu");
        assert_eq!(create, "Tạo lớp Chỉnh màu");
        assert_eq!(white_balance, "Cân bằng trắng");
    }
}
