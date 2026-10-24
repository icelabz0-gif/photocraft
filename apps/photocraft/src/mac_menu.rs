//! The macOS menu bar, through muda (a standalone native menu crate, not Tauri: no webview, just
//! AppKit menus behind a safe API, no GTK on macOS). The menus themselves, their Mac layout and how their keys reach PhotoCraft are in
//! `photocraft_ui_egui::native_menu`; this file only builds and updates the `NSMenu`s.
//!
//! - Item ids are command ids; a command in two menus (Keyboard Shortcuts is in Edit and Window)
//!   gets `id#2` for its second copy, as muda ids must be unique.
//! - A structure change (a new Open Recent file, a language switch) rebuilds the menu; anything
//!   else (labels, enabled, checked) is updated in place. Opening or closing a document rebuilds
//!   only the Window menu, whose list of documents changed: AppKit adds its own items to that
//!   menu (window tiling, the window list), so muda can't insert rows there by position.
//! - A chosen item is a click or a key equivalent: AppKit's current event says which. Key
//!   equivalents go back to egui as the key press that was made (the event's modifiers, not the
//!   item's: AppKit may match ⌘R to a ⇧⌘R item), so PhotoCraft's key rules still apply.
//! - Hide is a custom item calling `NSApplication hide:`, as the predefined one always takes ⌘H.
//!
//! **Context menus** (`photocraft_ui_egui::context_menu`) are `NSMenu`s popped up at the pointer,
//! with muda's items, so a choice arrives through the same `MenuEvent` handler. muda only pops a
//! menu up through an `unsafe` call (it takes a raw `NSView` pointer), and the workspace forbids
//! `unsafe`, so the menu goes the safe way round: built as a muda submenu, it is appended to the
//! menu bar just long enough to take its `NSMenu` back from `NSApp.mainMenu` and detach it, all
//! before AppKit draws again, and then `popUpMenuPositioningItem:atLocation:inView:` (safe in
//! objc2) shows it. That call runs AppKit's menu tracking loop until the menu closes, and winit
//! panics if its event handler is entered again meanwhile, so it runs from the main queue, once
//! winit's handler (and the egui frame) has returned.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, PoisonError};

use dispatch2::MainThreadBound;
use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSMenu};
use objc2_foundation::NSPoint;
use photocraft_ui_egui::native_menu::{self, Backend, Chord, Event, MenuBar, MenuRole, Node, Standard};

enum Handle {
    Plain(MenuItem),
    Check(CheckMenuItem),
}

/// One native item.
struct Entry {
    handle: Handle,
    /// The command id (several entries can share one).
    command: String,
    chord: Option<Chord>,
    label: String,
    enabled: bool,
}

/// How an item was chosen, read from AppKit's current event while it calls the handler.
struct Chosen {
    id: String,
    /// A key press (not a mouse click), and the modifiers held: ⌘, ⌃, ⌥, ⇧.
    key: bool,
    mods: [bool; 4],
}

enum Msg {
    Chosen(Chosen),
    /// Context menu `serial` closed.
    Closed(u64),
}

/// The context menu showing.
struct ContextMenu {
    serial: u64,
    /// Kept alive while it shows: dropping it cancels the menu.
    _menu: Submenu,
    /// Its items' context ids, by muda id.
    ids: HashMap<String, String>,
}

pub struct MacMenu {
    menu: Option<Menu>,
    /// By muda id.
    entries: HashMap<String, Entry>,
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    repaint: egui::Context,
    structure: u64,
    /// The Window menu, the muda ids of its items, and its [`native_menu::window_key`].
    window: Option<(Submenu, Vec<String>, u64)>,
    context: Option<ContextMenu>,
}

/// Where muda's event handler sends a choice: the newest [`MacMenu`]'s channel, and the context to
/// wake. muda keeps the first handler for the life of the process.
static CHOSEN: Mutex<Option<(Sender<Msg>, egui::Context)>> = Mutex::new(None);

impl MacMenu {
    pub fn new(ctx: &egui::Context) -> MacMenu {
        let (tx, rx) = channel();
        *CHOSEN.lock().unwrap_or_else(PoisonError::into_inner) = Some((tx.clone(), ctx.clone()));
        MenuEvent::set_event_handler(Some(|e: MenuEvent| {
            // AppKit calls this on the main thread, from the event that chose the item: a key
            // equivalent is a key-down, a click a mouse-up (or ↩ inside the open menu).
            let event = MainThreadMarker::new().and_then(|mtm| NSApplication::sharedApplication(mtm).currentEvent());
            let key = event.as_ref().is_some_and(|ev| ev.r#type() == NSEventType::KeyDown);
            let flags = event.as_ref().map(|ev| ev.modifierFlags()).unwrap_or(NSEventModifierFlags::empty());
            let mods = [NSEventModifierFlags::Command, NSEventModifierFlags::Control, NSEventModifierFlags::Option, NSEventModifierFlags::Shift]
                .map(|m| flags.contains(m));
            if let Some((tx, repaint)) = CHOSEN.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
                let _ = tx.send(Msg::Chosen(Chosen { id: e.id.0, key, mods }));
                // Wake egui, so the item runs now rather than on the next mouse move.
                repaint.request_repaint();
            }
        }));
        MacMenu { menu: None, entries: HashMap::new(), rx, tx, repaint: ctx.clone(), structure: 0, window: None, context: None }
    }

    /// Build context menu `serial` and take its `NSMenu`, detached from the menu bar (see the
    /// module docs). `None` before the menu bar is installed, or if AppKit's menu bar isn't ours.
    pub(crate) fn context_menu(&mut self, serial: u64, nodes: &[Node]) -> Option<Retained<NSMenu>> {
        let mtm = MainThreadMarker::new()?;
        let bar = self.menu.as_ref()?;
        // The title finds it in the menu bar; a popped-up menu shows no title.
        let title = format!("PhotoCraft context menu {serial}");
        let menu = Submenu::new(&title, true);
        let mut ids = HashMap::new();
        append_context(&menu, nodes, serial, &mut ids);
        bar.append(&menu).ok()?;
        let ns_bar = NSApplication::sharedApplication(mtm).mainMenu();
        let last = ns_bar.as_ref().map_or(0, |b| b.numberOfItems()) - 1;
        let host = ns_bar.filter(|_| last >= 0).and_then(|b| b.itemAtIndex(last));
        let ns_menu = host.as_ref().and_then(|h| h.submenu()).filter(|m| m.title().to_string() == title);
        if let (Some(host), Some(_)) = (&host, &ns_menu) {
            host.setSubmenu(None);
        }
        // Out of the menu bar again before AppKit draws it.
        let _ = bar.remove(&menu);
        let ns_menu = ns_menu?;
        self.context = Some(ContextMenu { serial, _menu: menu, ids });
        Some(ns_menu)
    }

    fn build(&mut self, bar: &MenuBar) {
        if let Some(old) = self.menu.take() {
            old.remove_for_nsapp();
        }
        self.entries.clear();
        self.window = None;
        let menu = Menu::new();
        let mut special = Vec::new();
        for m in &bar.menus {
            let (sub, ids) = self.submenu(m);
            let _ = menu.append(&sub);
            if m.role == MenuRole::Window {
                self.window = Some((sub.clone(), ids, native_menu::window_key(bar)));
            }
            if matches!(m.role, MenuRole::Window | MenuRole::Help) {
                special.push((m.role, sub));
            }
        }
        menu.init_for_nsapp();
        // Only once the menu is NSApp's: muda looks the submenus up in the installed main menu.
        for (role, sub) in special {
            match role {
                MenuRole::Window => sub.set_as_windows_menu_for_nsapp(),
                _ => sub.set_as_help_menu_for_nsapp(),
            }
        }
        self.menu = Some(menu);
        self.structure = native_menu::structure_key(bar);
    }

    /// A top-level menu, and the muda ids of the items it added.
    fn submenu(&mut self, m: &native_menu::Menu) -> (Submenu, Vec<String>) {
        let before: HashSet<String> = self.entries.keys().cloned().collect();
        let sub = Submenu::new(escape(&m.title), true);
        self.append(&sub, &m.children);
        let ids = self.entries.keys().filter(|id| !before.contains(*id)).cloned().collect();
        (sub, ids)
    }

    /// Replace the Window menu (a document opened or closed), leaving the other menus as they are.
    fn rebuild_window(&mut self, bar: &MenuBar) {
        let (Some(menu), Some((old, ids, _))) = (self.menu.clone(), self.window.take()) else { return self.build(bar) };
        let Some((at, spec)) = bar.menus.iter().enumerate().find(|(_, m)| m.role == MenuRole::Window) else { return self.build(bar) };
        if menu.remove(&old).is_err() {
            return self.build(bar);
        }
        for id in ids {
            self.entries.remove(&id);
        }
        let (sub, ids) = self.submenu(spec);
        if menu.insert(&sub, at).is_err() {
            return self.build(bar);
        }
        sub.set_as_windows_menu_for_nsapp();
        self.window = Some((sub, ids, native_menu::window_key(bar)));
    }

    fn append(&mut self, sub: &Submenu, nodes: &[Node]) {
        for n in nodes {
            match n {
                Node::Separator => {
                    let _ = sub.append(&PredefinedMenuItem::separator());
                }
                Node::Submenu { label, children, .. } => {
                    let s = Submenu::new(escape(label), !children.is_empty());
                    self.append(&s, children);
                    let _ = sub.append(&s);
                }
                Node::Standard(item) => {
                    let p = match item {
                        Standard::Services => PredefinedMenuItem::services(Some(photocraft_ui_egui::i18n::t("Services"))),
                        Standard::HideOthers => PredefinedMenuItem::hide_others(Some(photocraft_ui_egui::i18n::t("Hide Others"))),
                        Standard::ShowAll => PredefinedMenuItem::show_all(Some(photocraft_ui_egui::i18n::t("Show All"))),
                        Standard::Minimize => PredefinedMenuItem::minimize(None),
                        Standard::Zoom => PredefinedMenuItem::maximize(Some(photocraft_ui_egui::i18n::t("Zoom"))),
                        Standard::BringAllToFront => PredefinedMenuItem::bring_all_to_front(None),
                        Standard::CloseWindow => PredefinedMenuItem::close_window(None),
                    };
                    let _ = sub.append(&p);
                }
                Node::Item(it) => {
                    let id = self.unique_id(&it.id);
                    let chord = it.shortcut.as_deref().and_then(Chord::parse);
                    let accel = chord.as_ref().filter(|c| c.native_ok()).and_then(accelerator);
                    let handle = match it.checked {
                        Some(c) => {
                            let h = CheckMenuItem::with_id(id.clone(), escape(&it.label), it.enabled, c, accel);
                            let _ = sub.append(&h);
                            Handle::Check(h)
                        }
                        None => {
                            let h = MenuItem::with_id(id.clone(), escape(&it.label), it.enabled, accel);
                            let _ = sub.append(&h);
                            Handle::Plain(h)
                        }
                    };
                    self.entries.insert(id, Entry { handle, command: it.id.clone(), chord, label: it.label.clone(), enabled: it.enabled });
                }
            }
        }
    }

    /// muda ids must be unique: the second `edit.keyboardShortcuts` becomes `…#2`.
    fn unique_id(&self, id: &str) -> String {
        let mut unique = id.to_string();
        let mut n = 2;
        while self.entries.contains_key(&unique) {
            unique = format!("{id}#{n}");
            n += 1;
        }
        unique
    }
}

impl Backend for MacMenu {
    fn sync(&mut self, bar: &MenuBar) {
        if self.menu.is_none() || native_menu::structure_key(bar) != self.structure {
            self.build(bar);
            return;
        }
        if self.window.as_ref().map(|w| w.2) != Some(native_menu::window_key(bar)) {
            self.rebuild_window(bar);
        }
        let items: HashMap<&str, &native_menu::Item> = bar.items().into_iter().map(|it| (it.id.as_str(), it)).collect();
        for e in self.entries.values_mut() {
            let Some(it) = items.get(e.command.as_str()) else { continue };
            if e.label != it.label {
                let text = escape(&it.label);
                match &e.handle {
                    Handle::Plain(h) => h.set_text(text),
                    Handle::Check(h) => h.set_text(text),
                }
                e.label = it.label.clone();
            }
            if e.enabled != it.enabled {
                match &e.handle {
                    Handle::Plain(h) => h.set_enabled(it.enabled),
                    Handle::Check(h) => h.set_enabled(it.enabled),
                }
                e.enabled = it.enabled;
            }
            // Compare with AppKit's own state: muda toggles a check item when it's clicked.
            if let (Handle::Check(h), Some(c)) = (&e.handle, it.checked)
                && h.is_checked() != c
            {
                h.set_checked(c);
            }
        }
    }

    fn drain(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            let chosen = match msg {
                Msg::Chosen(chosen) => chosen,
                Msg::Closed(serial) => {
                    if self.context.as_ref().is_some_and(|c| c.serial == serial) {
                        self.context = None;
                    }
                    out.push(Event::ContextClosed(serial));
                    continue;
                }
            };
            if let Some(c) = &self.context
                && let Some(id) = c.ids.get(&chosen.id)
            {
                out.push(Event::ContextChosen { serial: c.serial, id: id.clone() });
                continue;
            }
            // Ids that aren't ours are AppKit's own items (Services, Zoom, …), already handled.
            let Some(e) = self.entries.get(&chosen.id) else { continue };
            if e.command == native_menu::HIDE {
                if let Some(mtm) = MainThreadMarker::new() {
                    NSApplication::sharedApplication(mtm).hide(None);
                }
                continue;
            }
            // A key-down chose it: its key equivalent, unless it was ↩ or Space inside the open
            // menu (no ⌘ or ⌃, and the item's shortcut needs one). The key is the item's (AppKit
            // matched on it); the modifiers are the ones held.
            let [cmd, ctrl, alt, shift] = chosen.mods;
            let key =
                e.chord.as_ref().filter(|c| chosen.key && (cmd || ctrl || !(c.cmd || c.ctrl))).map(|c| Chord { cmd, ctrl, alt, shift, key: c.key.clone() });
            out.push(match key {
                Some(chord) if e.command != native_menu::MINIMIZE => Event::Key(chord),
                _ => Event::Click(e.command.clone()),
            });
        }
        out
    }

    fn can_popup(&self) -> bool {
        true
    }

    fn popup(&mut self, serial: u64, nodes: &[Node], pos: [f32; 2]) -> bool {
        let Some(mtm) = MainThreadMarker::new() else { return false };
        let Some(ns_menu) = self.context_menu(serial, nodes) else { return false };
        let ns_menu = MainThreadBound::new(ns_menu, mtm);
        let (tx, repaint) = (self.tx.clone(), self.repaint.clone());
        let at = NSPoint::new(f64::from(pos[0]), f64::from(pos[1]));
        dispatch2::DispatchQueue::main().exec_async(move || {
            if let Some(mtm) = MainThreadMarker::new() {
                pop_up(&ns_menu.into_inner(mtm), mtm, at);
            }
            let _ = tx.send(Msg::Closed(serial));
            repaint.request_repaint();
        });
        true
    }
}

/// Show `menu` at `at` (points from the top left of the key window's content) until it closes.
fn pop_up(menu: &NSMenu, mtm: MainThreadMarker, at: NSPoint) {
    let app = NSApplication::sharedApplication(mtm);
    // winit's content view is flipped: its origin is the top left, as egui's.
    match app.keyWindow().or_else(|| app.mainWindow()).and_then(|w| w.contentView()) {
        Some(view) => menu.popUpMenuPositioningItem_atLocation_inView(None, at, Some(&view)),
        None => menu.popUpMenuPositioningItem_atLocation_inView(None, NSEvent::mouseLocation(), None),
    };
}

/// Context menu `serial`'s rows; muda ids `context.<serial>.<id>`, recorded in `ids`.
fn append_context(sub: &Submenu, nodes: &[Node], serial: u64, ids: &mut HashMap<String, String>) {
    for n in nodes {
        match n {
            Node::Separator => {
                let _ = sub.append(&PredefinedMenuItem::separator());
            }
            Node::Submenu { label, children, .. } => {
                let s = Submenu::new(escape(label), !children.is_empty());
                append_context(&s, children, serial, ids);
                let _ = sub.append(&s);
            }
            Node::Item(it) => {
                let id = format!("context.{serial}.{}", it.id);
                let accel = it.shortcut.as_deref().and_then(Chord::parse).filter(Chord::native_ok).as_ref().and_then(accelerator);
                let _ = match it.checked {
                    Some(c) => sub.append(&CheckMenuItem::with_id(id.clone(), escape(&it.label), it.enabled, c, accel)),
                    None => sub.append(&MenuItem::with_id(id.clone(), escape(&it.label), it.enabled, accel)),
                };
                ids.insert(id, it.id.clone());
            }
            Node::Standard(_) => {}
        }
    }
}

/// Install the menu bar now (in eframe's creator closure, so winit's default menu doesn't stay).
pub fn install(ctx: &egui::Context, app: &photocraft_ui_egui::PhotocraftApp) -> Option<native_menu::NativeMenu> {
    let language = &app.session.prefs().interface.language;
    let lang = photocraft_ui_egui::i18n::Lang::from_pref(language);
    let bar = native_menu::photocraft_layout(&photocraft_ui_egui::menus::menu_items(app), lang, language).bar;
    // Never crash for a menu: if AppKit refuses, keep the in-window menus.
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut menu = MacMenu::new(ctx);
        menu.sync(&bar);
        menu
    }));
    match built {
        Ok(menu) => Some(native_menu::NativeMenu::new(Box::new(menu))),
        Err(_) => {
            log::warn!("couldn't install the macOS menu bar; using the in-window menus");
            None
        }
    }
}

/// `&` marks a mnemonic in muda labels.
fn escape(s: &str) -> String {
    s.replace('&', "&&")
}

/// A key equivalent for AppKit. `Cmd` is ⌘ (`META`) and `Ctrl` is ⌃: never folded together.
fn accelerator(c: &Chord) -> Option<Accelerator> {
    let mut mods = Modifiers::empty();
    for (on, m) in [(c.cmd, Modifiers::META), (c.ctrl, Modifiers::CONTROL), (c.alt, Modifiers::ALT), (c.shift, Modifiers::SHIFT)] {
        if on {
            mods |= m;
        }
    }
    const LETTERS: [Code; 26] = [
        Code::KeyA,
        Code::KeyB,
        Code::KeyC,
        Code::KeyD,
        Code::KeyE,
        Code::KeyF,
        Code::KeyG,
        Code::KeyH,
        Code::KeyI,
        Code::KeyJ,
        Code::KeyK,
        Code::KeyL,
        Code::KeyM,
        Code::KeyN,
        Code::KeyO,
        Code::KeyP,
        Code::KeyQ,
        Code::KeyR,
        Code::KeyS,
        Code::KeyT,
        Code::KeyU,
        Code::KeyV,
        Code::KeyW,
        Code::KeyX,
        Code::KeyY,
        Code::KeyZ,
    ];
    const DIGITS: [Code; 10] =
        [Code::Digit0, Code::Digit1, Code::Digit2, Code::Digit3, Code::Digit4, Code::Digit5, Code::Digit6, Code::Digit7, Code::Digit8, Code::Digit9];
    const F: [Code; 24] = [
        Code::F1,
        Code::F2,
        Code::F3,
        Code::F4,
        Code::F5,
        Code::F6,
        Code::F7,
        Code::F8,
        Code::F9,
        Code::F10,
        Code::F11,
        Code::F12,
        Code::F13,
        Code::F14,
        Code::F15,
        Code::F16,
        Code::F17,
        Code::F18,
        Code::F19,
        Code::F20,
        Code::F21,
        Code::F22,
        Code::F23,
        Code::F24,
    ];
    let k = c.key.as_str();
    let mut chars = k.chars();
    let code = match (chars.next(), chars.next()) {
        (Some(ch), None) if ch.is_ascii_alphabetic() => *LETTERS.get((ch.to_ascii_uppercase() as usize).checked_sub('A' as usize)?)?,
        (Some(ch), None) if ch.is_ascii_digit() => *DIGITS.get((ch as usize).checked_sub('0' as usize)?)?,
        _ if c.is_function_key() => *F.get(k.get(1..)?.parse::<usize>().ok()?.checked_sub(1)?)?,
        _ => match k {
            "[" => Code::BracketLeft,
            "]" => Code::BracketRight,
            "\\" => Code::Backslash,
            "/" => Code::Slash,
            "=" => Code::Equal,
            "-" => Code::Minus,
            "'" => Code::Quote,
            ";" => Code::Semicolon,
            "," => Code::Comma,
            "." => Code::Period,
            "`" => Code::Backquote,
            "Delete" | "Backspace" => Code::Backspace,
            "Enter" if !mods.is_empty() => Code::Enter,
            "Tab" if !mods.is_empty() => Code::Tab,
            "Space" if !mods.is_empty() => Code::Space,
            _ => return None,
        },
    };
    Some(Accelerator::new(mods, code))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shortcut the Mac menu shows becomes a key equivalent (or stays egui's, for bare keys).
    #[test]
    fn every_menu_shortcut_maps_to_a_key_equivalent() {
        let app = photocraft_ui_egui::PhotocraftApp::new(photocraft_engine::Session::new(), photocraft_ui_egui::Services::default());
        let bar = native_menu::photocraft_layout(&photocraft_ui_egui::menus::menu_items(&app), photocraft_ui_egui::i18n::Lang::EN, "auto").bar;
        let mut native = 0;
        for it in bar.items() {
            let Some(c) = it.shortcut.as_deref().and_then(Chord::parse) else { continue };
            if c.native_ok() {
                assert!(accelerator(&c).is_some(), "{} ({}) has no key equivalent", c.portable(), it.id);
                native += 1;
            }
        }
        assert!(native > 70, "{native} key equivalents");
    }

    #[test]
    fn control_and_command_stay_distinct() {
        let a = accelerator(&Chord::parse("Cmd+M").unwrap()).unwrap();
        let b = accelerator(&Chord::parse("Ctrl+Cmd+M").unwrap()).unwrap();
        assert_ne!(a, b);
    }
}
