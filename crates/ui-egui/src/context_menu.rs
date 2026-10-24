//! Right-click menus: native menus where the app has them (the macOS desktop app's `NSMenu`s,
//! through the menu bar's backend, `native_menu`), egui popups everywhere else.
//!
//! A context menu is written once, against [`MenuUi`]: items, check items, separators and
//! submenus. Without native menus (Windows, Linux, the web, tests, `--in-window-menus`, or a
//! control channel attached, so agents can see, click and screenshot the menus) its calls draw
//! egui widgets in an egui popup.
//!
//! With them, a right-click runs the menu's code once to record it (every item reports "not
//! chosen") and the app pops the recorded [`Node`] tree up as a native menu after the frame. The
//! item chosen there comes back on a later frame, and the same code runs again with that item, and
//! only it, reporting the click: whatever the code does on a click (push an action, set a flag,
//! run a command) happens exactly as it does for an egui click. An item is matched by its place in
//! the menu and its label, so a menu whose rows changed in between does nothing rather than the
//! wrong thing. A menu with content only egui can draw ([`MenuUi::egui_only`]) stays an egui popup.
//!
//! [`show`] is `Response::context_menu`; [`show_native`] serves the menus the app opens and keeps in
//! its own state (the canvas menus, the text field menu), which it drops when the native menu
//! closes.

use egui::{Context, Id, Pos2, Response, Ui};

use crate::native_menu::{Item, Node};

/// One menu row: its label (what the native menu shows, and what a pick is matched by) and state.
#[derive(Clone, Copy, Debug)]
pub struct Row<'a> {
    label: &'a str,
    enabled: bool,
    checked: bool,
    shortcut: Option<&'a str>,
}

impl<'a> Row<'a> {
    pub fn new(label: &'a str) -> Row<'a> {
        Row { label, enabled: true, checked: false, shortcut: None }
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// A ✓ in the native menu (egui draws the check its own way, see [`MenuUi::row`]).
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    /// The keys that do the same (`Cmd+X`), shown in the native menu.
    pub fn shortcut(mut self, keys: &'a str) -> Self {
        self.shortcut = Some(keys).filter(|k| !k.is_empty());
        self
    }
}

/// The item chosen in a native menu: its id (its place in the menu, depth first) and label.
#[derive(Clone, Debug, PartialEq)]
struct Pick {
    id: String,
    label: String,
}

/// A native pass: records the menu, or (with a pick) reports the chosen item.
struct Recorder {
    /// The menu, then each submenu being recorded.
    levels: Vec<Vec<Node>>,
    next: usize,
    pick: Option<Pick>,
    /// Content only egui can draw was added: the menu has to be an egui popup.
    egui_only: bool,
}

impl Recorder {
    fn new(pick: Option<Pick>) -> Recorder {
        Recorder { levels: vec![Vec::new()], next: 0, pick, egui_only: false }
    }

    fn push(&mut self, node: Node) {
        if let Some(level) = self.levels.last_mut() {
            level.push(node);
        }
    }

    fn row(&mut self, row: Row<'_>) -> bool {
        let id = self.next.to_string();
        self.next += 1;
        let chosen = row.enabled && self.pick.as_ref().is_some_and(|p| p.id == id && p.label == row.label);
        self.push(Node::Item(Item {
            id,
            label: row.label.to_string(),
            shortcut: row.shortcut.map(str::to_string),
            enabled: row.enabled,
            checked: row.checked.then_some(true),
            role: None,
        }));
        chosen
    }

    fn submenu(&mut self, label: &str, add: impl FnOnce(&mut MenuUi)) {
        self.levels.push(Vec::new());
        add(&mut MenuUi { mode: Mode::Native(self) });
        let children = if self.levels.len() > 1 { self.levels.pop().unwrap_or_default() } else { Vec::new() };
        self.push(Node::Submenu { label: label.to_string(), children: crate::native_menu::tidy(children), role: None });
    }

    /// The recorded menu, or `None` when it has to be an egui popup (egui-only content, or nothing
    /// to choose).
    fn into_nodes(mut self) -> Option<Vec<Node>> {
        let nodes = crate::native_menu::tidy(self.levels.swap_remove(0));
        let any = nodes.iter().any(|n| matches!(n, Node::Item(_) | Node::Submenu { .. }));
        (!self.egui_only && any).then_some(nodes)
    }
}

enum Mode<'a> {
    Egui(&'a mut Ui),
    Native(&'a mut Recorder),
}

/// What a context menu's code adds its rows to: an egui popup's `Ui`, or a native menu.
pub struct MenuUi<'a> {
    mode: Mode<'a>,
}

impl<'a> MenuUi<'a> {
    /// Rows drawn into `ui` (a popup's): for menus that are only ever egui popups, and for code a
    /// panel menu and a context menu share.
    pub fn egui(ui: &'a mut Ui) -> MenuUi<'a> {
        MenuUi { mode: Mode::Egui(ui) }
    }

    /// The popup's `Ui`, for styling and layout; `None` for a native menu.
    pub fn ui(&mut self) -> Option<&mut Ui> {
        match &mut self.mode {
            Mode::Egui(ui) => Some(ui),
            Mode::Native(_) => None,
        }
    }

    /// A button; true (and the menu closes) when it is chosen.
    pub fn item(&mut self, label: &str) -> bool {
        self.item_enabled(true, label)
    }

    /// [`MenuUi::item`], greyed when not `enabled`.
    pub fn item_enabled(&mut self, enabled: bool, label: &str) -> bool {
        self.row(Row::new(label).enabled(enabled), |ui| ui.add_enabled(enabled, egui::Button::new(label)))
    }

    /// A check item: selected in egui (`selectable_label`), with a ✓ in a native menu.
    pub fn check(&mut self, checked: bool, label: &str) -> bool {
        self.row(Row::new(label).checked(checked), |ui| ui.selectable_label(checked, label))
    }

    /// `row`, which egui draws with `draw`; true (and the menu closes) when it is chosen.
    pub fn row(&mut self, row: Row<'_>, draw: impl FnOnce(&mut Ui) -> Response) -> bool {
        let chosen = self.row_open(row, draw);
        if chosen {
            self.close();
        }
        chosen
    }

    /// [`MenuUi::row`] that keeps an egui menu open when chosen (a native menu always closes).
    pub fn row_open(&mut self, row: Row<'_>, draw: impl FnOnce(&mut Ui) -> Response) -> bool {
        match &mut self.mode {
            Mode::Egui(ui) => draw(ui).clicked(),
            Mode::Native(r) => r.row(row),
        }
    }

    /// A check box, which egui draws with `draw` and keeps the menu open for; `value` flips when it
    /// is clicked or chosen. True when it flipped.
    pub fn toggle(&mut self, value: &mut bool, row: Row<'_>, draw: impl FnOnce(&mut Ui, &mut bool) -> Response) -> bool {
        match &mut self.mode {
            Mode::Egui(ui) => draw(ui, value).changed(),
            Mode::Native(r) => {
                let chosen = r.row(row.checked(*value));
                if chosen {
                    *value = !*value;
                }
                chosen
            }
        }
    }

    pub fn separator(&mut self) {
        match &mut self.mode {
            Mode::Egui(ui) => {
                ui.separator();
            }
            Mode::Native(r) => r.push(Node::Separator),
        }
    }

    /// A line of text that does nothing (a heading): greyed in a native menu.
    pub fn label(&mut self, text: &str) {
        match &mut self.mode {
            Mode::Egui(ui) => {
                ui.label(text);
            }
            Mode::Native(r) => {
                r.row(Row::new(text).enabled(false));
            }
        }
    }

    /// A submenu of `add`'s rows.
    pub fn submenu(&mut self, label: &str, add: impl FnOnce(&mut MenuUi)) {
        match &mut self.mode {
            Mode::Egui(ui) => {
                ui.menu_button(label, |ui| add(&mut MenuUi::egui(ui)));
            }
            Mode::Native(r) => r.submenu(label, add),
        }
    }

    /// `add`'s rows, scrolling in an egui popup that would run off the window
    /// (`widgets::menu_scroll`); a native menu scrolls by itself.
    pub fn scroll<R>(&mut self, add: impl FnOnce(&mut MenuUi) -> R) -> R {
        match &mut self.mode {
            Mode::Egui(ui) => crate::widgets::menu_scroll(ui, |ui| add(&mut MenuUi::egui(ui))),
            Mode::Native(r) => add(&mut MenuUi { mode: Mode::Native(r) }),
        }
    }

    /// The egui popup's minimum width.
    pub fn min_width(&mut self, width: f32) {
        if let Some(ui) = self.ui() {
            ui.set_min_width(width);
        }
    }

    /// Close an egui menu (a native menu closes by itself).
    pub fn close(&mut self) {
        if let Some(ui) = self.ui() {
            ui.close();
        }
    }

    /// Content only egui can draw (a slider, a text field): the menu stays an egui popup.
    pub fn egui_only(&mut self, add: impl FnOnce(&mut Ui)) {
        match &mut self.mode {
            Mode::Egui(ui) => add(ui),
            Mode::Native(r) => r.egui_only = true,
        }
    }
}

// ----------------------------------------------------------------------------- the app's side

/// A native menu to pop up after this frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    /// What opened it: the response (or the app's menu id) its pick goes back to.
    pub owner: Id,
    pub nodes: Vec<Node>,
    /// Where (points, in the window).
    pub pos: Pos2,
}

/// The native menus' state between frames, in the context's data (menus are built all over the
/// UI, and only the app holds the backend).
#[derive(Clone, Debug, Default)]
struct Host {
    /// Native menus are on this frame (set by the app each frame).
    enabled: bool,
    request: Option<Request>,
    /// The native menu showing: its owner.
    open: Option<Id>,
    /// A native menu that closed, its owner, and the frame it did.
    closed: Option<(Id, u64)>,
    /// The item chosen in a native menu, its owner, and the frame it came back in.
    pick: Option<(Id, Pick, u64)>,
}

fn with<R>(ctx: &Context, f: impl FnOnce(&mut Host) -> R) -> R {
    ctx.data_mut(|d| f(d.get_temp_mut_or_default::<Host>(Id::new("native-context-menu"))))
}

/// Are right-click menus native this frame?
pub fn native(ctx: &Context) -> bool {
    with(ctx, |h| h.enabled)
}

/// Native menus on or off from this frame on (the app, when it has a backend that pops them up).
pub(crate) fn set_native(ctx: &Context, on: bool) {
    with(ctx, |h| h.enabled = on);
}

/// The menu to pop up, if one was asked for.
pub(crate) fn take_request(ctx: &Context) -> Option<Request> {
    with(ctx, |h| h.request.take())
}

/// The item `id` labelled `label` was chosen in `owner`'s native menu.
pub(crate) fn chosen(ctx: &Context, owner: Id, id: String, label: String) {
    let frame = ctx.cumulative_frame_nr();
    with(ctx, |h| h.pick = Some((owner, Pick { id, label }, frame)));
}

/// `owner`'s native menu closed (after reporting its pick, if any).
pub(crate) fn closed(ctx: &Context, owner: Id) {
    let frame = ctx.cumulative_frame_nr();
    with(ctx, |h| {
        if h.open == Some(owner) {
            h.open = None;
        }
        h.closed = Some((owner, frame));
    });
}

/// Forget a pick or close no menu code took in the frame it came back in (its widget is gone).
pub(crate) fn expire(ctx: &Context) {
    let frame = ctx.cumulative_frame_nr();
    with(ctx, |h| {
        if h.pick.as_ref().is_some_and(|p| p.2 < frame) {
            h.pick = None;
        }
        if h.closed.is_some_and(|c| c.1 < frame) {
            h.closed = None;
        }
    });
}

fn take_pick(ctx: &Context, owner: Id) -> Option<Pick> {
    with(ctx, |h| match h.pick.take() {
        Some((o, pick, _)) if o == owner => Some(pick),
        other => {
            h.pick = other;
            None
        }
    })
}

/// Run `add` with `pick` reporting its click.
fn replay(pick: Pick, add: &mut impl FnMut(&mut MenuUi)) {
    let mut r = Recorder::new(Some(pick));
    add(&mut MenuUi { mode: Mode::Native(&mut r) });
}

/// Record `add`'s menu and ask for it as a native menu at `pos`; false when it has to be an egui
/// popup.
fn request(ctx: &Context, owner: Id, pos: Pos2, add: &mut impl FnMut(&mut MenuUi)) -> bool {
    let mut r = Recorder::new(None);
    add(&mut MenuUi { mode: Mode::Native(&mut r) });
    let Some(nodes) = r.into_nodes() else { return false };
    with(ctx, |h| {
        h.request = Some(Request { owner, nodes, pos });
        h.open = Some(owner);
        h.closed = None;
    });
    true
}

/// `resp`'s context menu (`Response::context_menu`): a native menu where the app has them.
pub fn show(resp: &Response, add: impl FnMut(&mut MenuUi)) {
    show_popup(resp, None, add);
}

/// [`show`] whose egui popup closes as `close` says (`CloseOnClickOutside`: toggling a check box
/// keeps it open). A native menu closes on any choice.
pub fn show_with(resp: &Response, close: egui::PopupCloseBehavior, add: impl FnMut(&mut MenuUi)) {
    show_popup(resp, Some(close), add);
}

fn show_popup(resp: &Response, close: Option<egui::PopupCloseBehavior>, mut add: impl FnMut(&mut MenuUi)) {
    let ctx = &resp.ctx;
    if native(ctx) {
        if let Some(pick) = take_pick(ctx, resp.id) {
            replay(pick, &mut add);
            return;
        }
        if resp.secondary_clicked() {
            let pos = resp.interact_pointer_pos().or_else(|| ctx.pointer_latest_pos()).unwrap_or(resp.rect.left_bottom());
            if request(ctx, resp.id, pos, &mut add) {
                return;
            }
        }
    }
    let popup = egui::Popup::context_menu(resp);
    let popup = match close {
        Some(close) => popup.close_behavior(close),
        None => popup,
    };
    popup.show(|ui| add(&mut MenuUi::egui(ui)));
}

/// A menu the app opened itself at `pos` and keeps in its own state (the canvas menus), as a
/// native menu: `None` when native menus are off or it has egui-only content (draw it with egui),
/// else whether it is still open. `Some(false)` comes once, when the native menu closed (after
/// `add` ran with the chosen item, if any): the app drops its state then.
pub fn show_native(ctx: &Context, owner: Id, pos: Pos2, mut add: impl FnMut(&mut MenuUi)) -> Option<bool> {
    if !native(ctx) {
        return None;
    }
    if let Some(pick) = take_pick(ctx, owner) {
        with(ctx, |h| h.closed = h.closed.filter(|c| c.0 != owner));
        replay(pick, &mut add);
        return Some(false);
    }
    let (open, closed) = with(ctx, |h| {
        let closed = h.closed.is_some_and(|c| c.0 == owner);
        if closed {
            h.closed = None;
        }
        (h.open == Some(owner), closed)
    });
    if closed {
        return Some(false);
    }
    if open {
        return Some(true);
    }
    request(ctx, owner, pos, &mut add).then_some(true)
}

/// Native menus open on the right button's press, as every Mac app's do, rather than on its
/// release as egui's click would. Wherever the app reads a right-click (`secondary_clicked`: the
/// panels, the canvas menus), so with native menus on, the press is completed into a click in the
/// same frame and the real release, if AppKit's menu doesn't take it, is dropped. A press that may
/// start a right-button drag (`drags`: brush resize, right-click erase) is left alone. Off when
/// menus are egui's: their right-click stays the release.
#[derive(Default)]
pub(crate) struct RightPress {
    /// A press was completed: drop the next right release.
    swallow: bool,
}

impl RightPress {
    pub(crate) fn complete(&mut self, ctx: &Context, events: &mut Vec<egui::Event>, drags: impl Fn(egui::Modifiers) -> bool) {
        use egui::{Event, PointerButton};
        if !native(ctx) {
            self.swallow = false;
            return;
        }
        let mut out = Vec::with_capacity(events.len() + 1);
        for e in events.drain(..) {
            match e {
                Event::PointerButton { pos, button: PointerButton::Secondary, pressed: true, modifiers } => {
                    // A new press: a release AppKit's menu took is not coming.
                    self.swallow = !drags(modifiers);
                    out.push(e);
                    if self.swallow {
                        out.push(Event::PointerButton { pos, button: PointerButton::Secondary, pressed: false, modifiers });
                    }
                }
                Event::PointerButton { button: PointerButton::Secondary, pressed: false, .. } if self.swallow => self.swallow = false,
                e => out.push(e),
            }
        }
        *events = out;
    }
}

/// The label of item `id` in `nodes`, at any depth.
pub(crate) fn label_of(nodes: &[Node], id: &str) -> Option<String> {
    nodes.iter().find_map(|n| match n {
        Node::Item(it) if it.id == id => Some(it.label.clone()),
        Node::Submenu { children, .. } => label_of(children, id),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A menu like the panels': items, a disabled one, a check, a submenu, a label.
    fn menu(m: &mut MenuUi, out: &mut Vec<&'static str>) {
        if m.item("Rename…") {
            out.push("rename");
        }
        m.separator();
        if m.item_enabled(false, "Merge Down") {
            out.push("merge");
        }
        if m.check(true, "Snap") {
            out.push("snap");
        }
        m.submenu("Color", |m| {
            m.label("Pick one");
            for name in ["Red", "Blue"] {
                if m.item(name) {
                    out.push(if name == "Red" { "red" } else { "blue" });
                }
            }
        });
        m.separator();
    }

    fn record(add: impl FnOnce(&mut MenuUi)) -> Option<Vec<Node>> {
        let mut r = Recorder::new(None);
        add(&mut MenuUi { mode: Mode::Native(&mut r) });
        r.into_nodes()
    }

    fn ids(nodes: &[Node]) -> Vec<String> {
        nodes
            .iter()
            .map(|n| match n {
                Node::Item(it) => format!("{}:{}{}{}", it.id, it.label, if it.enabled { "" } else { " (off)" }, if it.checked == Some(true) { " ✓" } else { "" }),
                Node::Separator => "---".into(),
                Node::Submenu { label, children, .. } => format!("[{label}: {}]", ids(children).join(", ")),
                Node::Standard(s) => format!("<{s:?}>"),
            })
            .collect()
    }

    #[test]
    fn recording_lists_the_rows_and_chooses_nothing() {
        let mut out = Vec::new();
        let nodes = record(|m| menu(m, &mut out)).unwrap();
        assert!(out.is_empty(), "recording ran a click: {out:?}");
        assert_eq!(ids(&nodes), ["0:Rename…", "---", "1:Merge Down (off)", "2:Snap ✓", "[Color: 3:Pick one (off), 4:Red, 5:Blue]"], "the trailing separator goes");
    }

    #[test]
    fn a_pick_clicks_that_item_only() {
        for (id, label, want) in [("0", "Rename…", vec!["rename"]), ("5", "Blue", vec!["blue"]), ("2", "Snap", vec!["snap"])] {
            let mut out = Vec::new();
            replay(Pick { id: id.into(), label: label.into() }, &mut |m: &mut MenuUi| menu(m, &mut out));
            assert_eq!(out, want);
        }
    }

    /// The rows changed between the pop-up and the pick: the item at that place isn't the one
    /// chosen, so nothing runs. A disabled item never runs.
    #[test]
    fn a_stale_or_disabled_pick_does_nothing() {
        for (id, label) in [("4", "Blue"), ("1", "Merge Down"), ("3", "Pick one"), ("99", "Red")] {
            let mut out = Vec::new();
            replay(Pick { id: id.into(), label: label.into() }, &mut |m: &mut MenuUi| menu(m, &mut out));
            assert!(out.is_empty(), "{id} {label}: {out:?}");
        }
    }

    #[test]
    fn egui_only_content_or_an_empty_menu_stays_an_egui_popup() {
        assert!(record(|m| {
            m.item("Reset");
            m.egui_only(|ui| {
                ui.label("a slider");
            });
        })
        .is_none());
        assert!(record(|m| m.separator()).is_none());
        assert!(record(|m| m.label("nothing to choose")).is_some(), "a greyed row still shows");
    }

    #[test]
    fn a_toggle_flips_its_value_when_chosen() {
        let mut on = false;
        let nodes = record(|m| {
            m.toggle(&mut on, Row::new("Shift"), |ui, v| ui.checkbox(v, "Shift"));
        })
        .unwrap();
        assert_eq!(ids(&nodes), ["0:Shift"]);
        replay(Pick { id: "0".into(), label: "Shift".into() }, &mut |m: &mut MenuUi| {
            m.toggle(&mut on, Row::new("Shift"), |ui, v| ui.checkbox(v, "Shift"));
        });
        assert!(on);
    }

    /// A right-click's frames: one to lay the widget out (egui hit-tests last frame's widgets),
    /// the press, then the release (egui clicks on the release).
    fn secondary_click(pos: Pos2) -> [egui::RawInput; 3] {
        let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE };
        [
            egui::RawInput::default(),
            egui::RawInput { events: vec![egui::Event::PointerMoved(pos), button(true)], ..Default::default() },
            egui::RawInput { events: vec![button(false)], ..Default::default() },
        ]
    }

    /// A right-click records the menu instead of opening egui's popup; the pick that comes back
    /// runs on the next frame, through the same code.
    #[test]
    fn show_asks_for_a_native_menu_and_runs_its_pick() {
        let ctx = Context::default();
        set_native(&ctx, true);
        let mut out = Vec::new();
        let frame = |ctx: &Context, input: egui::RawInput, out: &mut Vec<&'static str>| {
            let mut owner = None;
            ctx.run_ui(input, |ui| {
                let r = ui.allocate_rect(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 200.0)), egui::Sense::click());
                owner = Some(r.id);
                show(&r, |m| menu(m, out));
            })
            .drop_without_applying_deltas();
            owner
        };
        let [layout, press, release] = secondary_click(egui::pos2(40.0, 30.0));
        frame(&ctx, layout, &mut out);
        frame(&ctx, press, &mut out);
        let owner = frame(&ctx, release, &mut out).unwrap();
        let req = take_request(&ctx).expect("a native menu");
        assert_eq!((req.owner, req.pos), (owner, egui::pos2(40.0, 30.0)));
        assert!(!egui::Popup::is_any_open(&ctx), "no egui popup");
        assert!(out.is_empty());
        assert_eq!(label_of(&req.nodes, "4").as_deref(), Some("Red"));
        chosen(&ctx, owner, "4".into(), "Red".into());
        closed(&ctx, owner);
        frame(&ctx, egui::RawInput::default(), &mut out);
        assert_eq!(out, ["red"]);
        // The pick is used once.
        frame(&ctx, egui::RawInput::default(), &mut out);
        assert_eq!(out, ["red"]);
    }

    #[test]
    fn without_native_menus_show_opens_egui_popup() {
        let ctx = Context::default();
        let mut out = Vec::new();
        for input in secondary_click(egui::pos2(40.0, 30.0)).into_iter().chain([egui::RawInput::default()]) {
            ctx.run_ui(input, |ui| {
                let r = ui.allocate_rect(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 200.0)), egui::Sense::click());
                show(&r, |m| menu(m, &mut out));
            })
            .drop_without_applying_deltas();
        }
        assert!(take_request(&ctx).is_none());
        assert!(egui::Popup::is_any_open(&ctx));
    }

    /// The app's own menus: asked for once, open until the native menu closes, then closed once.
    #[test]
    fn show_native_follows_the_native_menu() {
        let ctx = Context::default();
        let owner = Id::new("canvas menu");
        let mut out = Vec::new();
        let frame = |out: &mut Vec<&'static str>| {
            let mut shown = None;
            ctx.run_ui(egui::RawInput::default(), |ui| shown = show_native(ui.ctx(), owner, egui::pos2(5.0, 6.0), |m| menu(m, out)))
                .drop_without_applying_deltas();
            shown
        };
        assert_eq!(frame(&mut out), None, "off: the app draws it with egui");
        set_native(&ctx, true);
        assert_eq!(frame(&mut out), Some(true));
        assert!(take_request(&ctx).is_some());
        assert_eq!(frame(&mut out), Some(true), "still open");
        assert!(take_request(&ctx).is_none(), "asked for once");
        // Closed without a pick.
        closed(&ctx, owner);
        assert_eq!(frame(&mut out), Some(false));
        assert!(out.is_empty());
        // Opened again, and an item chosen.
        assert_eq!(frame(&mut out), Some(true));
        take_request(&ctx).unwrap();
        chosen(&ctx, owner, "0".into(), "Rename…".into());
        closed(&ctx, owner);
        assert_eq!(frame(&mut out), Some(false));
        assert_eq!(out, ["rename"]);
    }

    /// A pick whose widget is gone isn't kept for a later menu.
    #[test]
    fn unclaimed_picks_expire() {
        let ctx = Context::default();
        let owner = Id::new("row");
        ctx.run_ui(egui::RawInput::default(), |ui| chosen(ui.ctx(), owner, "0".into(), "Rename…".into())).drop_without_applying_deltas();
        ctx.run_ui(egui::RawInput::default(), |ui| expire(ui.ctx())).drop_without_applying_deltas();
        assert!(take_pick(&ctx, owner).is_none());
    }

    fn right(pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::PointerButton { pos: egui::pos2(5.0, 5.0), button: egui::PointerButton::Secondary, pressed, modifiers }
    }

    /// With native menus a right press is a click at once (as AppKit opens menus on the press) and
    /// the release that follows is dropped; a press that may start a right-drag (Alt: brush
    /// resize) keeps its release. Egui's menus keep the release everywhere.
    #[test]
    fn native_menus_open_on_the_right_press() {
        let alt = egui::Modifiers::ALT;
        let none = egui::Modifiers::NONE;
        let drags = |m: egui::Modifiers| m.alt;
        let ctx = Context::default();
        let mut press = RightPress::default();
        let mut events = vec![right(true, none)];
        press.complete(&ctx, &mut events, drags);
        assert_eq!(events, [right(true, none)], "egui menus: untouched");
        let mut events = vec![right(false, none)];
        press.complete(&ctx, &mut events, drags);
        assert_eq!(events, [right(false, none)]);

        set_native(&ctx, true);
        let mut events = vec![right(true, none)];
        press.complete(&ctx, &mut events, drags);
        assert_eq!(events, [right(true, none), right(false, none)], "a click on the press");
        let mut events = vec![egui::Event::PointerMoved(egui::pos2(6.0, 6.0)), right(false, none)];
        press.complete(&ctx, &mut events, drags);
        assert_eq!(events, [egui::Event::PointerMoved(egui::pos2(6.0, 6.0))], "the real release is dropped");
        // AppKit's menu took the release: the next press starts afresh.
        for _ in 0..2 {
            let mut events = vec![right(true, none)];
            press.complete(&ctx, &mut events, drags);
            assert_eq!(events.len(), 2);
        }
        let mut events = vec![right(true, alt)];
        press.complete(&ctx, &mut events, drags);
        assert_eq!(events, [right(true, alt)], "a right-drag gesture keeps its press");
        let mut events = vec![right(false, alt)];
        press.complete(&ctx, &mut events, drags);
        assert_eq!(events, [right(false, alt)], "and its release");
    }
}
