use super::*;

#[test]
fn readable_types_best_first() {
    // Chrome dragging an image from a page.
    let chrome = ["text/x-moz-url", "UTF8_STRING", "text/plain", "XdndDirectSave0", "application/octet-stream", "chromium/x-renderer-taint", "text/html"];
    assert_eq!(readable(&chrome), vec![OCTET_STREAM, DIRECT_SAVE]);
    // An archive manager saves the file where it's told.
    assert_eq!(readable(&["XdndDirectSave0", "text/plain"]), vec![DIRECT_SAVE]);
    // Chrome's Downloads list.
    assert_eq!(readable(&["text/uri-list", "application/vnd.portal.files"]), vec![URI_LIST]);
    // Firefox dragging an image: its link first, then the pixels.
    assert_eq!(readable(&["text/x-moz-url", "text/uri-list", "image/jpeg", "image/png"]), vec![URI_LIST, "image/png", "image/jpeg"]);
    // Bytes without XdndDirectSave0 can be anything; a link alone opens nothing.
    assert!(readable(&["application/octet-stream", "text/plain"]).is_empty());
    assert!(readable(&["text/x-moz-url", "_NETSCAPE_URL"]).is_empty());
}

#[test]
fn uri_lists_give_their_local_files() {
    let crlf = b"file:///home/me/a.png\r\n# a comment\r\nfile:///home/me/b%20c.psd\r\n";
    assert_eq!(local_files(crlf), vec![PathBuf::from("/home/me/a.png"), PathBuf::from("/home/me/b c.psd")]);
    // Chromium separates its URIs with LF alone.
    assert_eq!(local_files(b"file:///tmp/a.png\nfile:///tmp/b.png"), vec![PathBuf::from("/tmp/a.png"), PathBuf::from("/tmp/b.png")]);
    assert!(local_files(b"https://example.com/a.png\r\n").is_empty());
    assert!(local_files(b"").is_empty());
    assert!(local_files(&[0xff, 0xfe, b'\n']).is_empty());
}

#[test]
fn dropped_data_is_named_by_a_file_name_never_a_path() {
    assert_eq!(data_name(Some("product.png"), OCTET_STREAM), PathBuf::from("product.png"));
    assert_eq!(data_name(Some("/home/me/.bashrc"), OCTET_STREAM), PathBuf::from(".bashrc"));
    assert_eq!(data_name(Some("../../etc/passwd"), "image/png"), PathBuf::from("passwd"));
    assert_eq!(data_name(Some("a\nb.png"), "image/png"), PathBuf::from("ab.png"));
    assert_eq!(data_name(Some(".."), "image/png"), PathBuf::from("Untitled.png"));
    assert_eq!(data_name(Some("  "), "image/webp"), PathBuf::from("Untitled.webp"));
    assert_eq!(data_name(None, OCTET_STREAM), PathBuf::from("Untitled"));
    assert!(data_name(Some("/abs.png"), OCTET_STREAM).is_relative());
}

#[test]
fn file_uris_round_trip() {
    let path = Path::new("/tmp/photocraft-drop-x/a b%é#?.png");
    let uri = file_uri(path);
    assert_eq!(uri, "file:///tmp/photocraft-drop-x/a%20b%25%C3%A9%23%3F.png");
    assert_eq!(crate::portal_save::file_uri_path(&uri).as_deref(), Some(path));
}

#[test]
fn hovering_shows_files_by_path_and_data_by_type() {
    let files = hovered(Some(&Offer::Files(vec![PathBuf::from("/tmp/a.png")])));
    assert_eq!(files, vec![egui::HoveredFile { path: Some(PathBuf::from("/tmp/a.png")), mime: String::new() }]);
    let data = hovered(Some(&Offer::Data(1, "image/png")));
    assert_eq!(data, vec![egui::HoveredFile { path: None, mime: "image/png".into() }]);
    assert!(hovered(None).is_empty());
}

#[test]
fn dropped_data_reads_its_bytes_or_its_error() {
    use egui::DroppedFile;
    let ok = DroppedData { name: PathBuf::from("a.png"), bytes: Ok(vec![1, 2, 3]) };
    assert_eq!((ok.path(), ok.bytes()), (Path::new("a.png"), Ok(vec![1, 2, 3])));
    assert_eq!(format!("{ok:?}"), r#"DroppedData { name: "a.png", bytes: Ok(3) }"#);
    let failed = DroppedData { name: PathBuf::from("a.png"), bytes: Err("stopped answering".into()) };
    assert_eq!(failed.bytes(), Err("stopped answering".into()));
}

#[test]
fn a_property_read_asks_for_no_more_than_the_budget_left() {
    assert_eq!(property_length(MAX_BYTES as usize), MAX_BYTES / 4);
    assert_eq!(property_length(5), 2, "rounded up to whole 32-bit units");
    assert_eq!(property_length(0), 0, "a spent budget reads only the empty end of a transfer");
    assert_eq!(property_length(usize::MAX), u32::MAX);
}

#[test]
fn a_direct_save_reads_only_a_plain_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.png");
    std::fs::write(&file, b"png").unwrap();
    assert_eq!(read_saved(&file), Ok(b"png".to_vec()));
    let link = dir.path().join("link.png");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    assert!(read_saved(&link).is_err(), "a link could point anywhere");
    assert!(read_saved(dir.path()).is_err(), "a folder");
    let fifo = dir.path().join("fifo.png");
    assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    assert!(read_saved(&fifo).is_err(), "a pipe is refused, not waited on");
    assert!(read_saved(&dir.path().join("missing.png")).is_err());
}

/// A drag source on its own X connection, as a browser is: it owns `XdndSelection` and sends
/// its data in the types it was given, in parts (INCR) when longer than `part`. With `saves`, it
/// writes that file where a Direct Save asks first, as an archive manager does. Its messages name
/// `target`: the app window (GTK, Qt) or the proxy (Chromium).
struct Source {
    conn: RustConnection,
    window: Window,
    target: Window,
    atoms: Atoms,
    data: Vec<(Atom, Vec<u8>)>,
    part: usize,
    saves: Option<Vec<u8>>,
}

impl Source {
    fn new(target: Window, data: &[(&str, &[u8])], part: usize) -> Self {
        let (conn, screen) = RustConnection::connect(None).unwrap();
        let root = conn.setup().roots[screen].root;
        let atoms = Atoms::new(&conn).unwrap().reply().unwrap();
        let window = conn.generate_id().unwrap();
        conn.create_window(0, window, root, 0, 0, 1, 1, 0, WindowClass::INPUT_ONLY, 0, &CreateWindowAux::new()).unwrap().check().unwrap();
        conn.set_selection_owner(window, atoms.XdndSelection, x11rb::CURRENT_TIME).unwrap().check().unwrap();
        let data = data.iter().map(|(t, d)| (conn.intern_atom(false, t.as_bytes()).unwrap().reply().unwrap().atom, d.to_vec())).collect();
        Self { conn, window, target, atoms, data, part, saves: None }
    }

    fn types(&self) -> Vec<Atom> {
        self.data.iter().map(|(t, _)| *t).collect()
    }

    fn send(&self, to: Window, kind: Atom, data: [u32; 5]) {
        self.conn.send_event(false, to, EventMask::NO_EVENT, ClientMessageEvent::new(32, self.target, kind, data)).unwrap();
        self.conn.flush().unwrap();
    }

    fn enter(&self, target: Window) {
        let types = self.types();
        let listed = if types.len() > 3 {
            self.conn.change_property32(PropMode::REPLACE, self.window, self.atoms.XdndTypeList, AtomEnum::ATOM, &types).unwrap().check().unwrap();
            [0; 3]
        } else {
            std::array::from_fn(|i| types.get(i).copied().unwrap_or(x11rb::NONE))
        };
        let more = u32::from(types.len() > 3);
        self.send(target, self.atoms.XdndEnter, [self.window, (XDND_VERSION << 24) | more, listed[0], listed[1], listed[2]]);
    }

    /// Send XdndPosition, serving data requests until the answer comes: its data.
    fn position(&self, target: Window) -> [u32; 5] {
        self.send(target, self.atoms.XdndPosition, [self.window, 0, (10 << 16) | 10, x11rb::CURRENT_TIME, self.atoms.XdndActionCopy]);
        self.serve_until(self.atoms.XdndStatus)
    }

    /// Send XdndDrop, serving data requests until XdndFinished comes: its data.
    fn drop_on(&self, target: Window) -> [u32; 5] {
        self.send(target, self.atoms.XdndDrop, [self.window, 0, x11rb::CURRENT_TIME, 0, 0]);
        self.serve_until(self.atoms.XdndFinished)
    }

    fn serve_until(&self, kind: Atom) -> [u32; 5] {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match self.conn.poll_for_event().unwrap() {
                Some(Event::ClientMessage(m)) if m.type_ == kind => return m.data.as_data32(),
                Some(Event::SelectionRequest(r)) => self.serve(&r),
                Some(_) => {}
                None => std::thread::sleep(Duration::from_millis(2)),
            }
        }
        panic!("no answer from the drop target");
    }

    fn serve(&self, r: &x11rb::protocol::xproto::SelectionRequestEvent) {
        if let Some(file) = self.saves.as_ref().filter(|_| r.target == self.atoms.XdndDirectSave0) {
            let uri = self.conn.get_property(false, self.window, self.atoms.XdndDirectSave0, AtomEnum::ANY, 0, 1024).unwrap().reply().unwrap().value;
            std::fs::write(crate::portal_save::file_uri_path(std::str::from_utf8(&uri).unwrap()).unwrap(), file).unwrap();
        }
        let data = self.data.iter().find(|(t, _)| *t == r.target).map(|(_, d)| d.clone());
        let property = if data.is_some() { r.property } else { x11rb::NONE };
        let incr = data.as_ref().is_some_and(|d| d.len() > self.part);
        if let Some(d) = &data {
            if incr {
                let aux = x11rb::protocol::xproto::ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE);
                self.conn.change_window_attributes(r.requestor, &aux).unwrap();
                self.conn.change_property32(PropMode::REPLACE, r.requestor, r.property, self.atoms.INCR, &[d.len() as u32]).unwrap();
            } else {
                self.conn.change_property8(PropMode::REPLACE, r.requestor, r.property, r.target, d).unwrap();
            }
        }
        let notify = x11rb::protocol::xproto::SelectionNotifyEvent {
            response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: r.time,
            requestor: r.requestor,
            selection: r.selection,
            target: r.target,
            property,
        };
        self.conn.send_event(false, r.requestor, EventMask::NO_EVENT, notify).unwrap();
        self.conn.flush().unwrap();
        let (Some(d), true) = (data, incr) else { return };
        // One part per deletion of the property, then an empty one.
        for part in d.chunks(self.part).chain([&[][..]]) {
            self.await_delete(r.requestor, r.property);
            self.conn.change_property8(PropMode::REPLACE, r.requestor, r.property, r.target, part).unwrap();
            self.conn.flush().unwrap();
        }
    }

    fn await_delete(&self, window: Window, property: Atom) {
        loop {
            if let Event::PropertyNotify(p) = self.conn.wait_for_event().unwrap()
                && p.window == window
                && p.atom == property
                && p.state == Property::DELETE
            {
                return;
            }
        }
    }
}

fn property32(conn: &RustConnection, window: Window, property: Atom) -> Vec<u32> {
    conn.get_property(false, window, property, AtomEnum::ANY, 0, 4).unwrap().reply().unwrap().value32().map(Iterator::collect).unwrap_or_default()
}

/// The protocol against a live X server, played by test sources that name the app window or the
/// proxy in their messages as GTK and Chromium do: Chrome dragging an image from
/// a page (more than three types, named by XdndDirectSave0), its Downloads list (file URIs on
/// LF-separated lines), large image data in parts, an archive manager's Direct Save, and a link
/// the app can't open.
#[test]
#[ignore = "requires a live X11 display: xvfb-run -a cargo test -p photocraft xdnd -- --ignored"]
fn browser_drags_land_on_a_live_x_server() {
    let (conn, screen) = RustConnection::connect(None).unwrap();
    let root = conn.setup().roots[screen].root;
    let atoms = Atoms::new(&conn).unwrap().reply().unwrap();
    let app = conn.generate_id().unwrap();
    conn.create_window(0, app, root, 0, 0, 100, 100, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new()).unwrap().check().unwrap();
    conn.flush().unwrap();
    let drops = start(app, || {}).unwrap();
    let proxy = property32(&conn, app, atoms.XdndProxy)[0];
    assert_eq!(property32(&conn, proxy, atoms.XdndProxy), vec![proxy], "the proxy names itself");
    assert_eq!(property32(&conn, proxy, atoms.XdndAware), vec![XDND_VERSION]);
    let take = || std::mem::take(&mut drops.lock().unwrap().dropped);
    let hovering = || drops.lock().unwrap().hovered.clone();

    // An image dragged from a web page in Chrome.
    let png = b"\x89PNG\r\n\x1a\n not really".to_vec();
    let chrome =
        Source::new(proxy, &[("text/x-moz-url", b"h\0"), ("text/plain", b"x"), ("XdndDirectSave0", b"F"), ("application/octet-stream", &png)], 1 << 20);
    chrome.conn.change_property8(PropMode::REPLACE, chrome.window, atoms.XdndDirectSave0, AtomEnum::STRING, b"web product.png").unwrap().check().unwrap();
    chrome.enter(proxy);
    let status = chrome.position(proxy);
    assert_eq!((status[0], status[1] & 1, status[4]), (proxy, 1, atoms.XdndActionCopy), "accepted as a copy, naming the window Chromium named");
    assert_eq!(hovering(), vec![egui::HoveredFile { path: None, mime: OCTET_STREAM.into() }]);
    let finished = chrome.drop_on(proxy);
    assert_eq!((finished[0], finished[1] & 1, finished[2]), (proxy, 1, atoms.XdndActionCopy));
    let dropped = take();
    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0].path(), Path::new("web product.png"));
    assert_eq!(dropped[0].bytes(), Ok(png));
    assert!(hovering().is_empty());

    // Two files from Chrome's Downloads list.
    let downloads = Source::new(proxy, &[("text/uri-list", b"file:///tmp/a%20b.png\nfile:///tmp/c.psd")], 1 << 20);
    downloads.enter(proxy);
    assert_eq!(downloads.position(proxy)[1] & 1, 1);
    assert_eq!(hovering().len(), 2);
    assert_eq!(downloads.drop_on(proxy)[1] & 1, 1);
    let paths: Vec<PathBuf> = take().iter().map(|f| f.path().to_path_buf()).collect();
    assert_eq!(paths, vec![PathBuf::from("/tmp/a b.png"), PathBuf::from("/tmp/c.psd")]);

    // Image data larger than the source sends at once (GTK sends over 256 KB in parts); a
    // web link beside it isn't a local file.
    let big: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
    let firefox = Source::new(app, &[("text/uri-list", b"https://example.com/a.png\r\n"), ("image/png", &big)], 64 * 1024);
    firefox.enter(proxy);
    let status = firefox.position(proxy);
    assert_eq!((status[0], status[1] & 1), (app, 1), "GTK's answers name the app window");
    let finished = firefox.drop_on(proxy);
    assert_eq!((finished[0], finished[1] & 1), (app, 1));
    let dropped = take();
    assert_eq!(dropped[0].path(), Path::new("Untitled.png"));
    assert_eq!(dropped[0].bytes().map(|b| b == big), Ok(true));

    // A file inside an archive (File Roller): saved where asked, read from there.
    let mut archive = Source::new(app, &[("XdndDirectSave0", b"S"), ("text/plain", b"")], 1 << 20);
    archive.saves = Some(big.clone());
    archive.conn.change_property8(PropMode::REPLACE, archive.window, atoms.XdndDirectSave0, AtomEnum::STRING, b"in archive.png").unwrap().check().unwrap();
    archive.enter(proxy);
    assert_eq!(archive.position(proxy)[1] & 1, 1);
    assert_eq!(archive.drop_on(proxy)[1] & 1, 1);
    let dropped = take();
    assert_eq!(dropped[0].path(), Path::new("in archive.png"));
    assert_eq!(dropped[0].bytes().map(|b| b == big), Ok(true));

    // A link: turned away, nothing hovers, nothing drops.
    let link = Source::new(app, &[("text/x-moz-url", b"h\0"), ("_NETSCAPE_URL", b"https://example.com")], 1 << 20);
    link.enter(proxy);
    let status = link.position(proxy);
    assert_eq!((status[1] & 1, status[4]), (0, x11rb::NONE));
    assert!(hovering().is_empty());
    assert_eq!(link.drop_on(proxy)[1] & 1, 0);
    assert!(take().is_empty());
}
