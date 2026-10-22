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

/// The chrome above the scrolling body: the title row, the hairline under it and the space
/// around them, with the spacing that follows it. The body's cap below adds this to the
/// window's height, so it is the measured height of that chrome, not a guess.
const HEADER_HEIGHT: f32 = 43.0;

/// The height of the panel's body: the mask controls, the three sections and the ten one-line
/// sliders (about 520 points). The body is this tall when the window has the room for it, and
/// shorter — scrolling, never clipping — when it has not. Ten two-line rows took about 840.
pub const BODY_HEIGHT: f32 = 520.0;

/// Headroom on top of [`BODY_HEIGHT`], so that a small change in the panel's content does not
/// start the body scrolling.
const BODY_HEADROOM: f32 = 20.0;
/// The panel's id, so the tests can find its rectangle in egui's memory.
fn panel_id() -> egui::Id {
    egui::Id::new("basic-color")
}

/// The panel's card, like Brush Settings' and Retouch AI's.
fn card_frame(t: &Tokens) -> egui::Frame {
    egui::Frame::NONE
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.card_border))
        .corner_radius(CornerRadius::same(t.radius_lg as u8))
        .shadow(egui::Shadow { offset: [0, 10], blur: 30, spread: 0, color: t.shadow })
        .inner_margin(egui::Margin::same(10))
}

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
    pub const PRESETS: &str = "Presets…";
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
        PRESETS,
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
    let frame = card_frame(&t);
    let canvas = app.last_canvas_rect;
    // The card's margin, on both sides: egui takes it off the requested minimum height before
    // that becomes the content minimum.
    let frame_margin = frame.total_margin().sum().y;
    // The body may not grow past the window: it is capped by the room below the panel's top
    // edge, which is where the panel opens (or where the user dragged it to).
    let default_top = canvas.top() + 24.0;
    let panel_top = ctx.memory(|mem| mem.area_rect(panel_id())).map_or(default_top, |rect| rect.top());
    let room = (ctx.content_rect().bottom() - panel_top.min(default_top) - frame_margin - HEADER_HEIGHT).max(0.0);
    let body_height = (BODY_HEIGHT + BODY_HEADROOM).min(room);
    egui::Window::new(tl!(text::TITLE))
        .id(panel_id())
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .frame(frame)
        .default_pos(egui::pos2((canvas.right() - WIDTH - 30.0).max(canvas.left() + 8.0), default_top))
        // The window keeps the room the body asks for: a borderless auto-sized window settles
        // for the height of its first frame and never grows, which cut the panel off after two
        // sliders. egui takes the frame's margin off the requested minimum before it becomes
        // the content minimum, so the request carries that margin again.
        .min_height(HEADER_HEIGHT + body_height + frame_margin)
        .show(ctx, |ui| {
            ui.set_width(WIDTH);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tl!(text::TITLE)).font(theme::semibold(13.0)).color(t.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::button(ui, "x", 20.0, false, tl!(text::CLOSE)).clicked() {
                        open = false;
                    }
                    // Color Presets lives in its own window; this only opens it, never toggles it off.
                    if widgets::compact_button(ui, tl!(text::PRESETS), 20.0).clicked() {
                        app.ui.panels.color_presets = true;
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
            let body = egui::ScrollArea::vertical().id_salt("basic-color-body").max_height(body_height).auto_shrink([false, true]).show(ui, |ui| {
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
            // What the tests measure: the body's viewport and the height of all its content, so
            // they can tell a body that scrolls from one that fits without reading pixels.
            #[cfg(test)]
            ui.ctx().data_mut(|d| d.insert_temp(panel_id().with("body"), (body.inner_rect, body.content_size)));
            #[cfg(not(test))]
            let _ = body;
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

    /// The ten sliders of the editor, in the order the panel shows them.
    const SLIDERS: &[&str] = &["Temperature", "Tint", "Exposure", "Contrast", "Highlights", "Shadows", "Whites", "Blacks", "Vibrance", "Saturation"];

    /// The three section headings of the editor.
    const SECTIONS: &[&str] = &["White Balance", "Tone", "Presence"];

    /// A harness with a document and a Basic Color layer, so the panel draws the editor.
    fn harness(size: egui::Vec2) -> egui_kittest::Harness<'static, PhotocraftApp> {
        let mut session = photocraft_engine::Session::new();
        session.execute("file.new", json!({"width": 64, "height": 64, "mode": "rgb", "depth": 8})).expect("new document");
        session.execute("layer.newAdjustmentLayer.basicColor", json!({})).expect("basic color layer");
        let mut app = PhotocraftApp::new(session, crate::Services::default());
        app.ui.panels.basic_color = true;
        let mut h = Harness::builder().with_size(size).build_ui_state(
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
        h
    }

    /// The panel's rectangle in the window.
    fn panel_rect(h: &egui_kittest::Harness<'static, PhotocraftApp>) -> egui::Rect {
        h.ctx.memory(|mem| mem.area_rect(panel_id())).expect("the panel is open")
    }

    /// The card's vertical margin: the frame's inner margin and its stroke, on both sides.
    fn frame_margin(h: &egui_kittest::Harness<'static, PhotocraftApp>) -> f32 {
        card_frame(&Tokens::get(&h.ctx)).total_margin().sum().y
    }

    /// The body's viewport and the size of everything inside it, recorded by [`window`]: a body
    /// whose content is taller than its viewport scrolls.
    fn body_metrics(h: &egui_kittest::Harness<'static, PhotocraftApp>) -> (egui::Rect, egui::Vec2) {
        h.ctx.data(|d| d.get_temp::<(egui::Rect, egui::Vec2)>(panel_id().with("body"))).expect("the body was drawn")
    }

    /// The panel must show every slider and every section heading when the window has the room
    /// for them: the body is as tall as the panel's content, so nothing needs scrolling.
    #[test]
    fn every_slider_is_visible_in_a_tall_window() {
        let h = harness(vec2(1440.0, 1200.0));
        let rect = panel_rect(&h);
        assert!(rect.bottom() <= 1200.0, "the panel stays inside the window: {rect:?}");
        for name in SLIDERS.iter().chain(SECTIONS) {
            let row = h.query_all_by_label(name).next().unwrap_or_else(|| panic!("the panel has a {name} row"));
            let row = row.rect();
            assert!(rect.contains_rect(row), "{name} is inside the panel: {row:?} in {rect:?}");
        }
        // The body ends with the last slider: the card is as tall as its content, not cut short.
        let last = h.query_all_by_label("Saturation").next().expect("the panel has a Saturation row").rect();
        assert!(rect.bottom() - last.bottom() < 60.0, "the body ends with the last slider: {last:?} in {rect:?}");
        // The card is the chrome plus the body, and the body keeps the height it asks for.
        let body = rect.height() - HEADER_HEIGHT - frame_margin(&h);
        assert!(body >= BODY_HEIGHT - 1.0, "the body keeps the height it asks for: {body} in {rect:?}");
    }

    /// Every one of the ten sliders and the three headings fits at a 1440×900 window, the size the
    /// app opens at: no row needs scrolling there.
    #[test]
    fn every_slider_is_visible_at_1440_by_900() {
        let h = harness(vec2(1440.0, 900.0));
        let rect = panel_rect(&h);
        assert!(rect.bottom() <= 900.0, "the panel stays inside the window: {rect:?}");
        let (body, content) = body_metrics(&h);
        assert!(content.y <= body.height() + 0.5, "the body does not scroll at 1440×900: {content:?} in {body:?}");
        for name in SLIDERS.iter().chain(SECTIONS) {
            let row = h.query_all_by_label(name).next().unwrap_or_else(|| panic!("the panel has a {name} row"));
            assert!(body.contains_rect(row.rect()), "{name} is inside the body: {:?} in {body:?}", row.rect());
        }
        // The last row is not at the edge: its 20-point slider and field are inside too.
        let last = h.query_all_by_label("Saturation").next().expect("the panel has a Saturation row").rect();
        assert!(body.bottom() - last.bottom() >= 4.0, "Saturation's row is fully visible: {last:?} in {body:?}");
    }

    /// The panel takes the room the window gives it and never grows past it: in a short window
    /// the body fills the space below the panel, in a tall one it is as tall as its content.
    #[test]
    fn the_panel_fills_the_window_without_growing_past_it() {
        for height in [600.0, 900.0, 1500.0] {
            let h = harness(vec2(1440.0, height));
            let rect = panel_rect(&h);
            assert!(rect.bottom() <= height, "the panel stays inside a {height} point window: {rect:?}");
            // The most the card asks for: the body plus its scroll headroom.
            let card = HEADER_HEIGHT + frame_margin(&h) + BODY_HEIGHT + BODY_HEADROOM;
            if height < rect.top() + card - 0.5 {
                assert!((rect.bottom() - height).abs() < 1.0, "the panel fills a {height} point window: {rect:?}");
            } else {
                // With the room for it the body is exactly as tall as its content (never past
                // the ask, never shorter than the height the constants promise).
                assert!(rect.height() <= card + 1.0, "the card stays within its ask in a {height} point window: {rect:?}");
                let body = rect.height() - HEADER_HEIGHT - frame_margin(&h);
                assert!(body >= BODY_HEIGHT - 1.0, "the body keeps the height it asks for in a {height} point window: {body} in {rect:?}");
            }
            // The card is WIDTH wide, plus the frame's margins.
            assert!((rect.width() - (WIDTH + frame_margin(&h))).abs() < 1.0, "the panel keeps its width: {rect:?}");
        }
    }

    /// A short window scrolls the body instead of clipping it: the last slider is out of view
    /// until the body is scrolled.
    #[test]
    fn a_short_window_scrolls_the_body() {
        let mut h = harness(vec2(1440.0, 600.0));
        let rect = panel_rect(&h);
        let (body, content) = body_metrics(&h);
        let saturation = h.query_all_by_label("Saturation").next().expect("the panel has a Saturation row");
        assert!(content.y > body.height() + 0.5, "the content is taller than the body: {content:?} in {body:?}");
        // The label is centred in its 20-point row, so the row runs about 3 points below it.
        assert!(saturation.rect().bottom() + 3.0 > body.bottom(), "the body is too short for every slider: {:?} in {body:?}", saturation.rect());
        // The wheel belongs to the scroll area under the pointer.
        h.hover_at(egui::pos2(rect.left() + 100.0, rect.top() + 120.0));
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, -400.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        h.run_steps(2);
        let (body, _) = body_metrics(&h);
        let saturation = h.query_all_by_label("Saturation").next().expect("the panel has a Saturation row");
        assert!(body.contains_rect(saturation.rect()), "scrolling brings the last slider into view: {:?} in {body:?}", saturation.rect());
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
