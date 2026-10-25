//! Magic Wand drag: the press selects as Photoshop's click does, and dragging away from it widens
//! the selection by colour, the way Pixelmator's wand does (Photoshop's wand ignores the drag).
//! The further the pointer gets from the press, in any direction, the higher the Tolerance;
//! coming back lowers it again, and the selection follows while the button is held. The options
//! bar shows the live Tolerance during the drag and goes back to its own value on release, so
//! every press starts from it.
//!
//! The whole drag is one "Magic Wand" history step: each update re-runs `select.magicWand` from
//! the press with the drag's `coalesce` key, and the engine combines it with the selection from
//! before the press (`selection_cmds::redrag_base`), so ⇧-drag adds and dragging back narrows.
//! The drag belongs to the document pressed on: once another is active it changes nothing. A
//! re-run that fails ends the live update, so no later frame retries it.

use serde_json::json;

use crate::PhotocraftApp;
use crate::canvas::{Drag, ViewXform};
use crate::state::Tool;

/// Tolerance levels per screen point between the pointer and the press.
const LEVELS_PER_POINT: f64 = 0.5;
/// Screen points the pointer may wander from the press before the Tolerance starts to follow.
const DEAD_ZONE: f64 = 3.0;
/// The shortest time between two re-runs, so a fast drag never queues work.
const MIN_INTERVAL: f64 = 1.0 / 30.0;

/// A Magic Wand press being dragged.
#[derive(Clone, Debug)]
pub struct WandDrag {
    /// The sampled pixel.
    seed: [f64; 2],
    /// The options-bar Tolerance at the press.
    base: f32,
    /// `select.magicWand`'s mode, from the press's modifiers and the mode buttons.
    mode: &'static str,
    document: Option<photocraft_doc::DocId>,
    /// The `coalesce` key shared by every re-run of this drag.
    key: String,
    /// The Tolerance the selection shows.
    applied: f32,
    /// Frame time of the last re-run, and how long it took (seconds).
    last_run: f64,
    cost: f64,
}

/// The press: select at the options-bar Tolerance, and track the drag that may follow.
pub(crate) fn press(app: &mut PhotocraftApp, x: f64, y: f64, pressure: f32, mods: egui::Modifiers) {
    // Every press gets its own key, even two in one frame (automation clicks).
    static PRESSES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let press = PRESSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let base = app.ui.tool_options.tolerance.clamp(0.0, 255.0);
    let mut wand = WandDrag {
        seed: [x.floor(), y.floor()],
        base,
        mode: crate::canvas::selection_mode(app, mods),
        document: active_document(app),
        key: format!("magicWand.drag.{press}"),
        applied: base,
        last_run: app.last_frame_time,
        cost: 0.0,
    };
    // A press that selects nothing it can work on (no pixels there) starts no drag.
    if run(app, &mut wand, base).is_err() {
        return;
    }
    let mut d = Drag::new(Tool::MagicWand, [x, y], vec![[x, y, f64::from(pressure)]], mods, false);
    d.wand = Some(wand);
    app.drag = Some(d);
}

/// Once a frame: follow the pointer of a held drag, at most every [`MIN_INTERVAL`] (or twice the
/// last re-run's cost on a big document).
pub(crate) fn flush(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some((wand, wanted)) = take_pending(app) else { return };
    let wait = MIN_INTERVAL.max(2.0 * wand.cost) - (app.last_frame_time - wand.last_run);
    if wait > 0.0 {
        if let Ok(after) = std::time::Duration::try_from_secs_f64(wait) {
            ctx.request_repaint_after(after);
        }
        put_back(app, wand);
    } else {
        follow(app, wand, wanted);
    }
}

/// Follow the pointer now, without waiting for the next frame: `ui.pointer` calls this after its
/// events, so its reply and the next screenshot show the drag where it is.
pub(crate) fn settle(app: &mut PhotocraftApp) {
    let Some((wand, wanted)) = take_pending(app) else { return };
    follow(app, wand, wanted);
}

/// The release: the selection takes the Tolerance where the pointer let go, and the options bar
/// goes back to the Tolerance the press started from.
pub(crate) fn finish(app: &mut PhotocraftApp, d: &Drag) {
    let Some(mut wand) = d.wand.clone() else { return };
    let wanted = wanted(app, d, wand.base);
    if wanted != wand.applied && wand.document == active_document(app) {
        apply(app, &mut wand, wanted);
    }
    app.ui.tool_options.tolerance = wand.base;
}

/// Re-run at the Tolerance the pointer asks for. After a failure the drag stops following the
/// pointer, and the options bar goes back to the press's Tolerance.
fn follow(app: &mut PhotocraftApp, mut wand: WandDrag, wanted: f32) {
    if apply(app, &mut wand, wanted) {
        put_back(app, wand);
    } else {
        app.ui.tool_options.tolerance = wand.base;
    }
}

/// [`run`], reporting a failure in the status bar: false when it failed. The selection keeps the
/// last Tolerance that worked.
fn apply(app: &mut PhotocraftApp, wand: &mut WandDrag, tolerance: f32) -> bool {
    let Err(e) = run(app, wand, tolerance) else { return true };
    app.ui.status = e;
    app.ui.status_error = true;
    false
}

/// The held drag's wand and the Tolerance the pointer asks for, when that differs from the one
/// shown. The wand is taken out of the drag; [`put_back`] returns it. A drag on a document that
/// is no longer active ends here.
fn take_pending(app: &mut PhotocraftApp) -> Option<(WandDrag, f32)> {
    let d = app.drag.as_ref().filter(|d| d.tool == Tool::MagicWand)?;
    let wand = d.wand.as_ref()?;
    if wand.document != active_document(app) {
        app.ui.tool_options.tolerance = wand.base;
        app.drag = None;
        return None;
    }
    let wanted = wanted(app, d, wand.base);
    if wanted == wand.applied {
        return None;
    }
    app.drag.as_mut()?.wand.take().map(|w| (w, wanted))
}

fn active_document(app: &PhotocraftApp) -> Option<photocraft_doc::DocId> {
    app.session.active().map(|st| st.doc.id)
}

/// The Tolerance `d`'s pointer asks for, from the press's `base`.
fn wanted(app: &PhotocraftApp, d: &Drag, base: f32) -> f32 {
    let last = d.points.last().map_or(d.start, |p| [p[0], p[1]]);
    tolerance_at(base, screen_distance(app, d.start, last))
}

/// Screen points from `start` to `at` (document pixels), as the view draws them (zoom and pixel
/// aspect).
fn screen_distance(app: &PhotocraftApp, start: [f64; 2], at: [f64; 2]) -> f64 {
    let d = [at[0] - start[0], at[1] - start[1]];
    match ViewXform::active(app) {
        Some(xf) => f64::from(xf.map_vec(egui::vec2(d[0] as f32, d[1] as f32)).length()),
        None => d[0].hypot(d[1]) * f64::from(app.point_zoom()),
    }
}

fn put_back(app: &mut PhotocraftApp, wand: WandDrag) {
    if let Some(d) = app.drag.as_mut().filter(|d| d.tool == Tool::MagicWand) {
        d.wand = Some(wand);
    }
}

/// Re-run the wand from the press at `tolerance`, and show that Tolerance in the options bar.
fn run(app: &mut PhotocraftApp, wand: &mut WandDrag, tolerance: f32) -> Result<(), String> {
    let o = &app.ui.tool_options;
    let params = json!({
        "x": wand.seed[0],
        "y": wand.seed[1],
        "tolerance": tolerance,
        "contiguous": o.contiguous,
        "antiAlias": o.anti_alias,
        "sampleAllLayers": o.sample_all.magic_wand,
        "mode": wand.mode,
        "coalesce": wand.key,
    });
    let started = crate::gpu_canvas::now_ms();
    app.run("select.magicWand", params)?;
    wand.cost = (crate::gpu_canvas::now_ms() - started) / 1000.0;
    wand.applied = tolerance;
    wand.last_run = app.last_frame_time;
    app.ui.tool_options.tolerance = tolerance;
    Ok(())
}

/// The Tolerance for the pointer `distance` screen points from the press: [`LEVELS_PER_POINT`]
/// more for every point, up to 255. Inside [`DEAD_ZONE`] the press's Tolerance holds.
fn tolerance_at(base: f32, distance: f64) -> f32 {
    if !distance.is_finite() || distance < DEAD_ZONE {
        return base;
    }
    (f64::from(base) + LEVELS_PER_POINT * distance).round().clamp(0.0, 255.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::{ToolEvent, tool_event};

    /// An 80×10 grey ramp, from level 200 at the left down 2 levels a column, with the Magic Wand
    /// at Tolerance 0 and no anti-aliasing.
    fn ramp_app() -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width": 80, "height": 10, "background": "white"})).unwrap();
        app.session
            .edit("paint", |doc, _| {
                let bg = doc.layers[0].surface_mut().unwrap();
                for x in 0..80 {
                    let v = (200.0 - 2.0 * x as f32) / 255.0;
                    bg.fill_rect(photocraft_geom::Rect::new(x, 0, x + 1, 10), &[v, v, v, 1.0]);
                }
                Ok(())
            })
            .unwrap();
        app.ui.tool = Tool::MagicWand;
        app.ui.tool_options.tolerance = 0.0;
        app.ui.tool_options.anti_alias = false;
        app
    }

    fn selected(app: &PhotocraftApp, x: i32) -> bool {
        app.session.active().unwrap().doc.selection.as_ref().is_some_and(|m| m.sample_channel(x, 5, 0) > 0.5)
    }

    fn steps(app: &PhotocraftApp) -> usize {
        app.session.active().unwrap().history.entries().len()
    }

    /// A pointer move to `points` screen points right of the press at column 0, then the frame's
    /// update, a second later so it isn't held back.
    fn drag_to(app: &mut PhotocraftApp, points: f64, mods: egui::Modifiers) {
        let x = 0.5 + points / f64::from(app.point_zoom());
        tool_event(app, ToolEvent::Move { x, y: 5.0, pressure: 1.0 }, mods);
        app.last_frame_time += 1.0;
        flush(app, &egui::Context::default());
    }

    #[test]
    fn dragging_right_widens_the_selection_and_dragging_back_narrows_it_in_one_step() {
        let mut app = ramp_app();
        let before = steps(&app);
        tool_event(&mut app, ToolEvent::Down { x: 0.5, y: 5.0, pressure: 1.0 }, egui::Modifiers::NONE);
        assert!(selected(&app, 0) && !selected(&app, 1), "the press selects at the options-bar Tolerance");
        // 60 points right: Tolerance 30, so columns within 30 levels (0–15).
        drag_to(&mut app, 60.0, egui::Modifiers::NONE);
        assert_eq!(app.ui.tool_options.tolerance, 30.0, "the options bar shows the live Tolerance");
        assert!(selected(&app, 15) && !selected(&app, 16));
        // Back to 10 points: Tolerance 5, columns 0–2.
        drag_to(&mut app, 10.0, egui::Modifiers::NONE);
        assert!(selected(&app, 2) && !selected(&app, 3), "dragging back narrows the selection");
        let x = 0.5 + 10.0 / f64::from(app.point_zoom());
        tool_event(&mut app, ToolEvent::Up { x, y: 5.0 }, egui::Modifiers::NONE);
        assert!(app.drag.is_none());
        assert_eq!(steps(&app), before + 1, "the whole drag is one history step");
        assert!(selected(&app, 2) && !selected(&app, 3), "the release keeps what the drag selected");
        assert_eq!(app.ui.tool_options.tolerance, 0.0, "the options bar goes back to the press's Tolerance");
        app.run("edit.undo", json!({})).unwrap();
        assert!(!selected(&app, 0), "one undo takes the whole drag back");
    }

    #[test]
    fn a_shift_drag_adds_to_the_selection_and_the_release_takes_the_last_pointer_position() {
        let mut app = ramp_app();
        // Select the far right column first.
        tool_event(&mut app, ToolEvent::Down { x: 79.5, y: 5.0, pressure: 1.0 }, egui::Modifiers::NONE);
        tool_event(&mut app, ToolEvent::Up { x: 79.5, y: 5.0 }, egui::Modifiers::NONE);
        assert!(selected(&app, 79));
        let shift = egui::Modifiers::SHIFT;
        tool_event(&mut app, ToolEvent::Down { x: 0.5, y: 5.0, pressure: 1.0 }, shift);
        drag_to(&mut app, 60.0, shift);
        assert!(selected(&app, 15) && selected(&app, 79), "⇧ adds to the selection from before the press");
        drag_to(&mut app, 10.0, shift);
        assert!(!selected(&app, 15) && selected(&app, 2) && selected(&app, 79));
        // A last move the frame never got to: the release applies it.
        let x = 0.5 + 40.0 / f64::from(app.point_zoom());
        tool_event(&mut app, ToolEvent::Move { x, y: 5.0, pressure: 1.0 }, shift);
        tool_event(&mut app, ToolEvent::Up { x, y: 5.0 }, shift);
        assert!(selected(&app, 10) && !selected(&app, 11) && selected(&app, 79), "Tolerance 20 at the release");
        assert_eq!(app.ui.tool_options.tolerance, 0.0);
    }

    #[test]
    fn ui_pointer_drives_the_drag_and_each_reply_shows_where_it_is() {
        let mut app = ramp_app();
        let ctx = egui::Context::default();
        let pointer = |app: &mut PhotocraftApp, events: serde_json::Value| {
            let (request, _reply) = crate::control::ControlRequest::new("ui.pointer", json!({ "events": events }));
            let _ = crate::control::handle(app, &ctx, &request);
        };
        let x = 0.5 + 60.0 / f64::from(app.point_zoom());
        pointer(&mut app, json!([{"kind": "down", "x": 0.5, "y": 5}]));
        pointer(&mut app, json!([{"kind": "move", "x": x, "y": 5}]));
        assert!(selected(&app, 15) && !selected(&app, 16), "a move shows at once, no frame needed");
        assert_eq!(app.ui.tool_options.tolerance, 30.0);
        pointer(&mut app, json!([{"kind": "up", "x": x, "y": 5}]));
        assert!(selected(&app, 15) && app.drag.is_none());
        assert_eq!(app.ui.tool_options.tolerance, 0.0);
    }

    #[test]
    fn a_drag_changes_nothing_once_another_document_is_active() {
        let mut app = ramp_app();
        tool_event(&mut app, ToolEvent::Down { x: 0.5, y: 5.0, pressure: 1.0 }, egui::Modifiers::NONE);
        app.run("file.new", json!({"width": 80, "height": 10, "background": "white"})).unwrap();
        let other = steps(&app);
        drag_to(&mut app, 60.0, egui::Modifiers::NONE);
        assert!(app.drag.is_none(), "the drag ends");
        assert_eq!(app.ui.tool_options.tolerance, 0.0, "the options bar goes back to the press's Tolerance");
        assert!(!selected(&app, 0) && steps(&app) == other, "the other document is untouched");
        app.session.set_active(0);
        assert!(selected(&app, 0) && !selected(&app, 1), "the first keeps what the press selected");
    }

    #[test]
    fn the_release_on_another_document_changes_nothing_there() {
        let mut app = ramp_app();
        tool_event(&mut app, ToolEvent::Down { x: 0.5, y: 5.0, pressure: 1.0 }, egui::Modifiers::NONE);
        app.run("file.new", json!({"width": 80, "height": 10, "background": "white"})).unwrap();
        let other = steps(&app);
        let x = 0.5 + 60.0 / f64::from(app.point_zoom());
        tool_event(&mut app, ToolEvent::Move { x, y: 5.0, pressure: 1.0 }, egui::Modifiers::NONE);
        tool_event(&mut app, ToolEvent::Up { x, y: 5.0 }, egui::Modifiers::NONE);
        assert!(!selected(&app, 0) && steps(&app) == other);
        assert_eq!(app.ui.tool_options.tolerance, 0.0);
    }

    #[test]
    fn a_refused_update_ends_the_live_drag_and_no_later_frame_retries_it() {
        let mut app = ramp_app();
        let before = steps(&app);
        tool_event(&mut app, ToolEvent::Down { x: 0.5, y: 5.0, pressure: 1.0 }, egui::Modifiers::NONE);
        app.session.authorize = Some(|_, _| Err(photocraft_engine::EngineError::Other("test rejection".into())));
        drag_to(&mut app, 60.0, egui::Modifiers::NONE);
        assert!(app.ui.status_error, "the failure shows in the status bar");
        assert_eq!(app.ui.tool_options.tolerance, 0.0);
        app.session.authorize = None;
        drag_to(&mut app, 80.0, egui::Modifiers::NONE);
        let x = 0.5 + 80.0 / f64::from(app.point_zoom());
        tool_event(&mut app, ToolEvent::Up { x, y: 5.0 }, egui::Modifiers::NONE);
        assert!(selected(&app, 0) && !selected(&app, 1), "the selection keeps the press's Tolerance");
        assert_eq!(steps(&app), before + 1, "only the press made a step");
        assert_eq!(app.ui.tool_options.tolerance, 0.0);
    }

    #[test]
    fn a_click_selects_as_before() {
        let mut app = ramp_app();
        app.ui.tool_options.tolerance = 4.0;
        let before = steps(&app);
        tool_event(&mut app, ToolEvent::Down { x: 0.5, y: 5.0, pressure: 1.0 }, egui::Modifiers::NONE);
        tool_event(&mut app, ToolEvent::Up { x: 0.5, y: 5.0 }, egui::Modifiers::NONE);
        assert!(selected(&app, 2) && !selected(&app, 3));
        assert_eq!(app.ui.tool_options.tolerance, 4.0);
        assert_eq!(steps(&app), before + 1);
    }

    #[test]
    fn a_stretched_view_measures_the_drag_as_drawn() {
        let mut app = ramp_app();
        app.ui.view.pixel_aspect_correction = true;
        app.ui.view.pixel_aspect = "anamorphic2To1".into();
        tool_event(&mut app, ToolEvent::Down { x: 0.5, y: 5.0, pressure: 1.0 }, egui::Modifiers::NONE);
        // 30 points of document x, drawn twice as wide: 60 points on screen, Tolerance 30.
        drag_to(&mut app, 30.0, egui::Modifiers::NONE);
        assert_eq!(app.ui.tool_options.tolerance, 30.0);
        assert!(selected(&app, 15) && !selected(&app, 16));
    }

    #[test]
    fn screen_distance_follows_the_zoom_in_any_direction() {
        let mut app = ramp_app();
        let zoom = f64::from(app.point_zoom());
        let s = [10.0, 5.0];
        assert!((screen_distance(&app, s, [10.0 + 30.0 / zoom, 5.0 + 40.0 / zoom]) - 50.0).abs() < 1e-3);
        assert!((screen_distance(&app, s, [10.0 - 40.0 / zoom, 5.0]) - 40.0).abs() < 1e-3);
        app.ui.view.flip_horizontal = true;
        assert!((screen_distance(&app, s, [10.0 - 40.0 / zoom, 5.0]) - 40.0).abs() < 1e-3, "a flipped view measures the same");
        assert!(screen_distance(&app, s, [f64::NAN, 5.0]).is_nan());
    }

    #[test]
    fn tolerance_grows_with_the_distance_from_the_press_and_stays_in_range() {
        assert_eq!(tolerance_at(32.0, 1.4), 32.0, "a click's wobble keeps the Tolerance");
        assert_eq!(tolerance_at(32.0, 40.0), 52.0);
        assert_eq!(tolerance_at(32.0, 50.0), 57.0);
        assert_eq!(tolerance_at(32.0, 2000.0), 255.0);
        assert_eq!(tolerance_at(32.0, f64::NAN), 32.0);
        assert_eq!(tolerance_at(32.0, f64::INFINITY), 32.0);
    }
}
