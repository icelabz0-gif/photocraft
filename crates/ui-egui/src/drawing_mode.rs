//! Tablet drawing mode: a canvas-first shell for illustration and comics.
//!
//! The normal desktop chrome stays untouched underneath; this module draws a small set of
//! floating controls over the canvas instead. The mode itself is
//! [`Interface::drawing_mode`](photocraft_engine::prefs::Interface), so the Android adapter can
//! turn it on for a tablet's first launch and the S Pen button can toggle it at any time.

use egui::{Align2, Color32, CornerRadius, Id, Order, Stroke, vec2};

use crate::PhotocraftApp;
use crate::state::{DrawingModeUi, Tool};

/// The tools offered by the compact palette, in the order a comic artist reaches for them.
const PALETTE_TOOLS: [Tool; 8] = [Tool::Brush, Tool::Eraser, Tool::Lasso, Tool::RectMarquee, Tool::Move, Tool::Eyedropper, Tool::Hand, Tool::Zoom];

/// Draw the floating drawing-mode controls. Call once per frame after the canvas.
pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let t = crate::theme::Tokens::get(ctx);
    let left_handed = app.session.prefs().interface.left_handed;
    let frame = || {
        egui::Frame::new()
            .fill(t.dock)
            .stroke(Stroke::new(1.0, t.card_border))
            .corner_radius(CornerRadius::same(t.radius_lg as u8))
            .inner_margin(egui::Margin::same(6))
    };

    // The palette sits opposite the drawing hand. Only this palette is mirrored; the full
    // desktop layout keeps its usual sides.
    let (palette_align, palette_offset) = if left_handed { (Align2::RIGHT_CENTER, vec2(-12.0, 0.0)) } else { (Align2::LEFT_CENTER, vec2(12.0, 0.0)) };
    egui::Area::new(Id::new("drawing-tools")).order(Order::Foreground).anchor(palette_align, palette_offset).show(ctx, |ui| {
        frame().show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            for tool in PALETTE_TOOLS {
                let selected = app.ui.tool == tool;
                let tip = tl!(tool.label());
                let response = crate::icons::button(ui, crate::icons::tool_icon(tool), 44.0, selected, tip);
                if response.clicked() {
                    crate::tool_memory::select(app, tool);
                }
            }
        });
    });

    // A small handle on the side opposite the palette returns to the full interface. The S Pen
    // button does the same thing; this is the touch fallback.
    let (handle_align, handle_offset) = if left_handed { (Align2::LEFT_TOP, vec2(12.0, 12.0)) } else { (Align2::RIGHT_TOP, vec2(-12.0, 12.0)) };
    egui::Area::new(Id::new("drawing-exit")).order(Order::Foreground).anchor(handle_align, handle_offset).show(ctx, |ui| {
        frame().show(ui, |ui| {
            let icon = if left_handed { "chevrons-right" } else { "chevrons-left" };
            let response = crate::icons::button(ui, icon, 36.0, false, tl!("Show/Hide All Panels"));
            if response.clicked() {
                let _ = crate::menus::invoke(app, ctx, "view.drawingMode", serde_json::json!({}));
            }
        });
    });

    // Brush size and opacity are the two controls a comic artist changes constantly. They stay
    // on the bottom edge, within thumb reach, together with undo/redo, color and layers. The row
    // wraps and shrinks on narrow viewports (a phone in portrait) so none of these is pushed off
    // screen; on a tablet it stays a single row.
    crate::paint_mouse::sync_tool_brush(app);
    let before = app.session.tools.brush.clone();
    let mut brush = before.clone();
    let narrow = ctx.content_rect().width() < 640.0;
    let button = if narrow { 38.0 } else { 42.0 };
    let size_width = if narrow { 150.0 } else { 180.0 };
    let opacity_width = if narrow { 120.0 } else { 150.0 };
    egui::Area::new(Id::new("drawing-bar")).order(Order::Foreground).anchor(Align2::CENTER_BOTTOM, vec2(0.0, -14.0)).show(ctx, |ui| {
        frame().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(if narrow { 4.0 } else { 6.0 }, 4.0);

                if crate::icons::button(ui, "undo-2", button, false, tl!("Undo")).clicked() {
                    let _ = crate::menus::invoke(app, ctx, "edit.undo", serde_json::json!({}));
                }
                if crate::icons::button(ui, "redo-2", button, false, tl!("Redo")).clicked() {
                    let _ = crate::menus::invoke(app, ctx, "edit.redo", serde_json::json!({}));
                }

                ui.add_sized([size_width, button], egui::Slider::new(&mut brush.size, 1.0..=500.0).show_value(true).text(tl!("Size")));
                let mut opacity = brush.opacity * 100.0;
                if ui.add_sized([opacity_width, button], egui::Slider::new(&mut opacity, 0.0..=100.0).show_value(true).text(tl!("Opacity"))).changed() {
                    brush.opacity = opacity / 100.0;
                }

                // Foreground color swatch. The color sheet carries the full picker.
                let fg = app.session.tools.foreground;
                let color = Color32::from_rgba_unmultiplied((fg[0] * 255.0) as u8, (fg[1] * 255.0) as u8, (fg[2] * 255.0) as u8, (fg[3] * 255.0) as u8);
                let color_clicked = ui
                    .scope(|ui| {
                        ui.spacing_mut().interact_size = vec2(button, button);
                        crate::widgets::color_swatch_button(ui, color, tl!("Color")).clicked()
                    })
                    .inner;
                if color_clicked {
                    app.ui.drawing.color = !app.ui.drawing.color;
                    app.ui.drawing.layers = false;
                }

                if crate::icons::button(ui, "layers", button, false, tl!("Layers")).clicked() {
                    app.ui.drawing.layers = !app.ui.drawing.layers;
                    app.ui.drawing.color = false;
                }

                // Pick a different brush preset without leaving the canvas. The picker opens
                // above the quick bar; its own gear opens the full Brush Settings panel.
                if app.ui.tool.is_brushlike() {
                    let response = crate::icons::button(ui, "brush", button, false, tl!("Brush Preset picker"));
                    if response.clicked() {
                        crate::brush_picker::open(&mut app.ui, ctx, [response.rect.left(), response.rect.top() - 440.0]);
                    }
                }
            });
        });
    });

    if brush.size != before.size || brush.opacity != before.opacity {
        crate::brush_panel::commit_gesture(app, ctx, &before, &brush);
    }

    show_sheet(app, ctx, &frame);
}

/// The Layers or Color sheet beside the compact palette. Both stay on the right edge: the
/// left-handed preference mirrors the palette, not the full desktop dock layout.
fn show_sheet(app: &mut PhotocraftApp, ctx: &egui::Context, frame: &impl Fn() -> egui::Frame) {
    let (group, title) = match (app.ui.drawing.layers, app.ui.drawing.color) {
        (true, _) => (crate::dock::Group::Layers, tl!("Layers")),
        (_, true) => (crate::dock::Group::Color, tl!("Color")),
        _ => return,
    };
    egui::Area::new(Id::new("drawing-sheet"))
        .order(Order::Foreground)
        // Leave the right-edge tool palette visible beside the sheet.
        .anchor(Align2::RIGHT_CENTER, vec2(-68.0, 0.0))
        .show(ctx, |ui| {
            frame().show(ui, |ui| {
                ui.set_width(300.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(title).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if crate::icons::button(ui, "x", 30.0, false, tl!("Close")).clicked() {
                            app.ui.drawing.layers = false;
                            app.ui.drawing.color = false;
                        }
                    });
                });
                egui::ScrollArea::vertical().max_height(520.0).auto_shrink([false, true]).show(ui, |ui| {
                    // The Color tab's index depends on the theme: Color is first in the Pro
                    // layout and second in the standard one. Ask the same source `dock_body` uses.
                    let pro = crate::theme::Tokens::get(ctx).pro;
                    let tab = group.tabs(pro).iter().position(|name| *name == "Color").unwrap_or(0);
                    crate::panels::dock_body(app, ui, group, tab);
                });
            });
        });
}

/// The drawing mode's transient sheet state, reset when leaving the mode.
pub fn reset_sheets(ui: &mut DrawingModeUi) {
    ui.layers = false;
    ui.color = false;
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_color_sheet_finds_the_color_tab_in_both_theme_layouts() {
        for pro in [true, false] {
            let groups = crate::dock::Group::Color.tabs(pro);
            let color = groups.iter().position(|name| *name == "Color");
            assert_eq!(color, Some(if pro { 0 } else { 1 }));
        }
    }
}
