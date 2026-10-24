//! The macOS menu bar end to end, on the main thread (`harness = false`): install it through the
//! real muda backend (`src/mac_menu.rs`), read AppKit's menu bar back, open a document and see the
//! items update in place, then choose items the two ways AppKit can: a key equivalent (a real ⌘R
//! key-down through `-[NSMenu performKeyEquivalent:]`) must reach egui as the key press it was, and
//! a click (`-[NSMenu performActionForItemAtIndex:]`) must run as a click, also when the click is
//! ↩ inside the open menu.

// The app's own module, built into this test (cargo builds harness = false tests with cfg(test), so its
// unit tests compile here too without a harness to run them).
#[cfg(target_os = "macos")]
#[path = "../src/mac_menu.rs"]
#[allow(unused_imports, dead_code)]
mod mac_menu;

#[cfg(target_os = "macos")]
fn main() {
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSApplication, NSEvent, NSEventMask, NSEventModifierFlags, NSEventSubtype, NSEventType, NSMenu};
    use objc2_foundation::{NSPoint, NSString};
    use photocraft_ui_egui::PhotocraftApp;
    use photocraft_ui_egui::native_menu;
    use serde_json::json;

    // Without a harness nothing answers cargo-nextest's `--list`, so it ran the whole test as its
    // listing and failed on the output. List the one test, as libtest's terse format would, and
    // nothing for `--ignored` (nextest asks twice). Running it passes `<name> --exact`, which falls
    // through to the test below.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--list") {
        if !args.iter().any(|a| a == "--ignored") {
            println!("mac_menu_appkit: test");
        }
        return;
    }

    let mtm = MainThreadMarker::new().expect("harness = false runs on the main thread");
    let nsapp = NSApplication::sharedApplication(mtm);
    nsapp.finishLaunching();

    let titles = |menu: &NSMenu| -> Vec<String> {
        (0..menu.numberOfItems()).filter_map(|i| menu.itemAtIndex(i)).filter(|it| !it.isSeparatorItem()).map(|it| it.title().to_string()).collect()
    };
    let submenu = |menu: &NSMenu, title: &str| -> Retained<NSMenu> {
        (0..menu.numberOfItems())
            .filter_map(|i| menu.itemAtIndex(i))
            .find(|it| it.title().to_string() == title)
            .and_then(|it| it.submenu())
            .unwrap_or_else(|| panic!("no {title} submenu in {:?}", titles(menu)))
    };
    let index = |menu: &NSMenu, title: &str| -> isize {
        (0..menu.numberOfItems())
            .find(|&i| menu.itemAtIndex(i).is_some_and(|it| it.title().to_string() == title))
            .unwrap_or_else(|| panic!("no {title} in {:?}", titles(menu)))
    };
    let main_menu = || nsapp.mainMenu().expect("a main menu");
    // Make `event` AppKit's current event, as the run loop does before dispatching it.
    let dequeue = |event: &NSEvent| {
        nsapp.postEvent_atStart(event, true);
        let mode = NSString::from_str("kCFRunLoopDefaultMode");
        let got = nsapp.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::Any, None, &mode, true).expect("the posted event");
        assert_eq!(got.r#type(), event.r#type());
        got
    };
    let key_down = |chars: &str, code: u16, flags: NSEventModifierFlags| {
        let s = NSString::from_str(chars);
        NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            NSEventType::KeyDown,
            NSPoint::new(0.0, 0.0),
            flags,
            0.0,
            0,
            None,
            &s,
            &s,
            false,
            code,
        )
        .expect("a key event")
    };
    let other = || {
        NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
            NSEventType::ApplicationDefined,
            NSPoint::new(0.0, 0.0),
            NSEventModifierFlags::empty(),
            0.0,
            0,
            None,
            NSEventSubtype::ApplicationActivated.0,
            0,
            0,
        )
        .expect("an application-defined event")
    };

    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), photocraft_ui_egui::Services::default());
    let ctx = egui::Context::default();
    let frame = |app: &mut PhotocraftApp, raw: egui::RawInput| {
        let mut out = ctx.run_ui(raw, |ui| {
            native_menu::run(app, ui.ctx());
            photocraft_ui_egui::shortcuts::handle(app, ui.ctx());
            native_menu::sync(app, ui.ctx());
        });
        out.textures_delta.clear();
    };
    app.services.native_menu = mac_menu::install(&ctx, &app);
    assert!(app.services.native_menu.is_some(), "the menu bar installed");

    // The Mac layout, as AppKit has it.
    let bar = main_menu();
    let tops: Vec<String> = (0..bar.numberOfItems()).filter_map(|i| bar.itemAtIndex(i)?.submenu()).map(|m| m.title().to_string()).collect();
    assert_eq!(tops, ["PhotoCraft", "File", "Edit", "Image", "Layer", "Type", "Select", "Filter", "View", "Window", "Help"]);
    let app_menu = submenu(&bar, "PhotoCraft");
    let names = titles(&app_menu);
    for want in ["About PhotoCraft", "Settings", "Language", "Appearance", "Hide PhotoCraft", "Quit PhotoCraft"] {
        assert!(names.iter().any(|n| n == want), "{want} missing from the app menu: {names:?}");
    }
    let quit = app_menu.itemAtIndex(index(&app_menu, "Quit PhotoCraft")).expect("Quit");
    assert_eq!(quit.keyEquivalent().to_string(), "q");
    assert_eq!(quit.keyEquivalentModifierMask(), NSEventModifierFlags::Command);
    assert!(!titles(&submenu(&bar, "File")).iter().any(|n| n == "Exit"), "Exit moved to the app menu as Quit");
    assert_eq!(nsapp.windowsMenu().map(|m| m.title().to_string()).as_deref(), Some("Window"), "AppKit knows the Window menu");
    assert_eq!(nsapp.helpMenu().map(|m| m.title().to_string()).as_deref(), Some("Help"), "AppKit knows the Help menu");

    // Opening a document enables Layer › New › Layer… in place: same NSMenu, no rebuild.
    let new_layer = |bar: &NSMenu| {
        let new = submenu(&submenu(bar, "Layer"), "New");
        new.itemAtIndex(index(&new, "Layer…")).expect("Layer…").isEnabled()
    };
    assert!(!new_layer(&bar), "no document: New Layer is disabled");
    app.run("file.new", json!({"width": 64, "height": 48})).expect("a document");
    frame(&mut app, egui::RawInput::default());
    assert!(Retained::as_ptr(&bar) == Retained::as_ptr(&main_menu()), "opening a document updated the menu, not rebuilt it");
    assert!(new_layer(&main_menu()), "with a document New Layer is enabled");

    // A key equivalent: ⌘R reaches egui as ⌘R and toggles View › Rulers once.
    let rulers = app.ui.extras.rulers;
    let press = dequeue(&key_down("r", 15, NSEventModifierFlags::Command));
    assert!(main_menu().performKeyEquivalent(&press), "⌘R is View › Rulers' key equivalent");
    let mut raw = egui::RawInput::default();
    if let Some(m) = app.services.native_menu.as_mut() {
        m.raw_input(&mut raw);
    }
    let keys: Vec<_> = raw
        .events
        .iter()
        .filter_map(|e| if let egui::Event::Key { key, pressed, modifiers, .. } = e { Some((*key, *pressed, modifiers.command)) } else { None })
        .collect();
    // AppKit took only the key-down; winit delivers the key-up itself (#1638).
    assert_eq!(keys, [(egui::Key::R, true, true)], "the key press");
    frame(&mut app, raw);
    let ran: Vec<String> = photocraft_ui_egui::shortcut_dispatch::take_log(&ctx).into_iter().map(|(id, _)| id).collect();
    assert_eq!(ran, ["view.rulers"], "⌘R ran Rulers, whichever ⌘R item AppKit matched");
    assert_eq!(app.ui.extras.rulers, !rulers, "⌘R toggled Rulers once");

    // A click: View › Rulers chosen with the mouse runs the command.
    let view = submenu(&main_menu(), "View");
    let at = index(&view, "Rulers");
    dequeue(&other());
    view.performActionForItemAtIndex(at);
    let mut raw = egui::RawInput::default();
    if let Some(m) = app.services.native_menu.as_mut() {
        m.raw_input(&mut raw);
    }
    assert!(raw.events.is_empty(), "a click is no key press: {:?}", raw.events);
    frame(&mut app, raw);
    assert_eq!(app.ui.extras.rulers, rulers, "the click toggled Rulers back");

    // ↩ inside the open menu is a key-down without ⌘: still a click.
    dequeue(&key_down("\r", 36, NSEventModifierFlags::empty()));
    view.performActionForItemAtIndex(at);
    let mut raw = egui::RawInput::default();
    if let Some(m) = app.services.native_menu.as_mut() {
        m.raw_input(&mut raw);
    }
    assert!(raw.events.is_empty(), "↩ in the menu is a click: {:?}", raw.events);
    frame(&mut app, raw);
    assert_eq!(app.ui.extras.rulers, !rulers, "↩ on Rulers toggled it");

    // Window ends with the open documents (#2991). Opening one rebuilds only the Window menu,
    // which AppKit still knows as its Window menu; choosing a row brings that document forward.
    app.run("file.new", json!({"width": 32, "height": 32})).expect("a second document");
    frame(&mut app, egui::RawInput::default());
    assert!(Retained::as_ptr(&bar) == Retained::as_ptr(&main_menu()), "a new document rebuilt only the Window menu");
    let window = submenu(&main_menu(), "Window");
    assert!(nsapp.windowsMenu().is_some_and(|w| Retained::as_ptr(&w) == Retained::as_ptr(&window)), "AppKit knows the new Window menu");
    let names: Vec<String> = app.session.documents().iter().map(|d| d.doc.name.clone()).collect();
    // The last rows (both documents are "Untitled").
    let rows = [window.numberOfItems() - 2, window.numberOfItems() - 1];
    let shown: Vec<String> = rows.iter().filter_map(|&i| window.itemAtIndex(i)).map(|it| it.title().to_string()).collect();
    assert_eq!(shown, names, "in tab order, after Bring All to Front: {:?}", titles(&window));
    let checked = |menu: &NSMenu, at: isize| menu.itemAtIndex(at).is_some_and(|it| it.state() == 1);
    assert!(!checked(&window, rows[0]) && checked(&window, rows[1]), "the active document is checked");
    dequeue(&other());
    window.performActionForItemAtIndex(rows[0]);
    let mut raw = egui::RawInput::default();
    if let Some(m) = app.services.native_menu.as_mut() {
        m.raw_input(&mut raw);
    }
    frame(&mut app, raw);
    assert_eq!(app.session.active_index(), Some(0), "choosing the first row brought it forward");
    let window = submenu(&main_menu(), "Window");
    assert!(checked(&window, rows[0]) && !checked(&window, rows[1]), "the check moved in place");

    // A context menu: built as a submenu of the menu bar only long enough to take its NSMenu back,
    // so the menu bar is as it was and the menu has no parent to pop up from. Choosing an item
    // reports it as the context menu's, not as a menu bar click.
    use photocraft_ui_egui::native_menu::{Backend, Event, Item, Node};
    let mut backend = mac_menu::MacMenu::new(&ctx);
    backend.sync(&native_menu::photocraft_layout(&photocraft_ui_egui::menus::menu_items(&app), photocraft_ui_egui::i18n::Lang::EN, "auto").bar);
    let tops_before = titles(&main_menu());
    let item = |id: &str, label: &str, checked: Option<bool>| {
        Node::Item(Item { id: id.into(), label: label.into(), shortcut: (id == "0").then(|| "Cmd+X".into()), enabled: id != "1", checked, role: None })
    };
    let nodes = vec![
        item("0", "Cut", None),
        item("1", "Merge Down", None),
        Node::Separator,
        item("2", "Snap", Some(true)),
        Node::Submenu { label: "Color".into(), children: vec![item("3", "Red & Blue", None)], role: None },
    ];
    let menu = backend.context_menu(7, &nodes).expect("a context menu");
    assert_eq!(titles(&main_menu()), tops_before, "the menu bar is as it was");
    let bar = main_menu();
    let held = (0..bar.numberOfItems()).filter_map(|i| bar.itemAtIndex(i)?.submenu()).any(|m| Retained::as_ptr(&m) == Retained::as_ptr(&menu));
    assert!(!held, "detached from the menu bar");
    assert_eq!(titles(&menu), ["Cut", "Merge Down", "Snap", "Color"]);
    let cut = menu.itemAtIndex(0).expect("Cut");
    assert_eq!((cut.keyEquivalent().to_string().as_str(), cut.keyEquivalentModifierMask()), ("x", NSEventModifierFlags::Command), "shortcuts show");
    assert!(!menu.itemAtIndex(1).expect("Merge Down").isEnabled());
    assert_eq!(menu.itemAtIndex(3).expect("Snap").state(), 1, "checked");
    assert_eq!(titles(&submenu(&menu, "Color")), ["Red & Blue"], "& is no mnemonic");
    dequeue(&other());
    submenu(&menu, "Color").performActionForItemAtIndex(0);
    assert_eq!(backend.drain(), [Event::ContextChosen { serial: 7, id: "3".into() }]);
    dequeue(&other());
    menu.performActionForItemAtIndex(0);
    assert_eq!(backend.drain(), [Event::ContextChosen { serial: 7, id: "0".into() }]);

    // The real pop-up: AppKit's menu tracking loop, run from the main queue once the frame is done.
    // ↓ and ↩ wait in the event queue, so the menu chooses its first item and closes by itself.
    assert!(backend.popup(8, &nodes, [10.0, 10.0]), "popped up");
    assert!(backend.drain().is_empty(), "nothing yet: it shows after the frame");
    for (chars, code) in ["\u{f701}", "\r"].into_iter().zip([125, 36]) {
        nsapp.postEvent_atStart(&key_down(chars, code, NSEventModifierFlags::empty()), false);
    }
    let mode = NSString::from_str("kCFRunLoopDefaultMode");
    let mut got = Vec::new();
    for _ in 0..200 {
        objc2_foundation::NSRunLoop::mainRunLoop().runMode_beforeDate(&mode, &objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05));
        got.extend(backend.drain());
        if got.contains(&Event::ContextClosed(8)) {
            break;
        }
    }
    assert_eq!(got, [Event::ContextChosen { serial: 8, id: "0".into() }, Event::ContextClosed(8)], "the choice, then the close");

    println!("mac_menu_appkit: ok");
}

#[cfg(not(target_os = "macos"))]
fn main() {}
