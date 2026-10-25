//! Drag and drop onto the window on X11, received here rather than by winit, so drags from
//! browsers land.
//!
//! winit 0.30's X11 drop target reads only `text/uri-list` and answers every drag with
//! `XdndActionPrivate`, which Chromium takes for "no drop": an image dragged from a web page (no
//! file list) and a file dragged from Chrome's Downloads (the answer) both came to nothing, while
//! file managers' drags opened. Photoshop opens both.
//!
//! This module's drop target is an unmapped window on a second, pure-Rust X connection; the app
//! window's `XdndProxy` property sends every source's XDND messages to it instead of winit
//! (XDND v5, "Proxy windows"). Its answers name the window each message named: GTK and Qt
//! name the app window, Chromium the proxy, and each ignores answers naming the other. It
//! accepts with `XdndActionCopy` and takes, best first:
//!
//! - local files from a `text/uri-list` (file managers, Chrome's Downloads), by path, as before;
//! - the file of an image dragged from a page: Chromium offers the bytes it downloaded as
//!   `application/octet-stream` beside `XdndDirectSave0`, named in that property of its window;
//! - image data (`image/png` and kin: Firefox, GIMP);
//! - a file the source saves where it's told (Direct Save, `XdndDirectSave0` alone: archive
//!   managers), read from a private folder.
//!
//! They reach the UI as egui's hovered and dropped files ([`FileDropsFn`]), so they go where
//! winit's drops go: placed on the canvas, opened at a slot of the tab strip, opened elsewhere.
//! Data without a file arrives under a file name only, so it opens as a new, unsaved document.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use eframe::wgpu::rwh::{HasWindowHandle, RawWindowHandle};
use photocraft_ui_egui::FileDropsFn;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, GetPropertyReply, PropMode, Property, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        XdndAware,
        XdndProxy,
        XdndEnter,
        XdndPosition,
        XdndStatus,
        XdndLeave,
        XdndDrop,
        XdndFinished,
        XdndSelection,
        XdndTypeList,
        XdndActionCopy,
        XdndDirectSave0,
        INCR,
        PHOTOCRAFT_DROP,
        TextPlain: b"text/plain",
        OctetStream: b"application/octet-stream",
    }
}

/// The XDND version this target speaks.
const XDND_VERSION: u32 = 5;
const URI_LIST: &str = "text/uri-list";
const OCTET_STREAM: &str = "application/octet-stream";
const DIRECT_SAVE: &str = "XdndDirectSave0";
/// Image data types read when a drag brings no file, best first: PNG keeps alpha and every pixel.
const IMAGE_TYPES: [&str; 6] = ["image/png", "image/tiff", "image/webp", "image/jpeg", "image/gif", "image/bmp"];
/// The most a drop's data may hold.
const MAX_BYTES: u32 = 1 << 30;
/// The most types read from a source's `XdndTypeList`.
const MAX_TYPES: u32 = 256;
/// How long a source gets to send each part of a drop's data.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a source gets to save a file where it was told (an archive manager extracting it).
const SAVE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a whole transfer may take, however steadily its parts arrive.
const MAX_TRANSFER_TIME: Duration = Duration::from_secs(120);
/// The most events kept for later while a transfer is waited for.
const MAX_PENDING: usize = 4096;

/// The drop target for the app's window, when eframe runs on X11 (`None` elsewhere, or when it
/// can't be set up: winit's own target then stays in charge).
pub fn service(cc: &eframe::CreationContext<'_>) -> Option<FileDropsFn> {
    let window = match cc.window_handle().ok()?.as_raw() {
        RawWindowHandle::Xlib(h) => u32::try_from(h.window).ok()?,
        RawWindowHandle::Xcb(h) => h.window.get(),
        _ => return None,
    };
    let ctx = cc.egui_ctx.clone();
    let drops = start(window, move || ctx.request_repaint()).map_err(|e| log::warn!("drops from browsers won't open: {e}")).ok()?;
    Some(Box::new(move |raw: &mut egui::RawInput| {
        let mut drops = drops.lock().unwrap_or_else(PoisonError::into_inner);
        raw.hovered_files.extend(drops.hovered.iter().cloned());
        raw.dropped_files.append(&mut drops.dropped);
    }))
}

/// What the UI takes each frame: the files over the window now and those dropped since.
#[derive(Default)]
struct Drops {
    hovered: Vec<egui::HoveredFile>,
    dropped: Vec<egui::DroppedFileHandle>,
}

/// Start receiving the drops for `window` on a background thread, which calls `wake` whenever
/// [`Drops`] changed. The thread ends when the X connection closes.
fn start(window: Window, wake: impl Fn() + Send + 'static) -> Result<Arc<Mutex<Drops>>, String> {
    let (conn, screen) = RustConnection::connect(None).map_err(|e| format!("cannot connect to the X server: {e}"))?;
    let root = conn.setup().roots.get(screen).map(|s| s.root).ok_or_else(|| format!("X screen {screen} not found"))?;
    let atoms = Atoms::new(&conn).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?;
    let proxy = conn.generate_id().map_err(|e| e.to_string())?;
    let aux = CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE);
    conn.create_window(0, proxy, root, -1, -1, 1, 1, 0, WindowClass::INPUT_ONLY, 0, &aux).map_err(|e| e.to_string())?;
    conn.change_property32(PropMode::REPLACE, proxy, atoms.XdndAware, AtomEnum::ATOM, &[XDND_VERSION]).map_err(|e| e.to_string())?;
    // Qt follows a proxy only when it names itself.
    conn.change_property32(PropMode::REPLACE, proxy, atoms.XdndProxy, AtomEnum::WINDOW, &[proxy]).map_err(|e| e.to_string())?;
    // Last: from here on, sources talk to `proxy` and winit hears no drags.
    conn.change_property32(PropMode::REPLACE, window, atoms.XdndProxy, AtomEnum::WINDOW, &[proxy])
        .map_err(|e| e.to_string())?
        .check()
        .map_err(|e| format!("XdndProxy: {e}"))?;
    let drops = Arc::new(Mutex::new(Drops::default()));
    let receiver = Receiver { conn, atoms, proxy, drops: drops.clone(), wake: Box::new(wake), drag: None, pending: VecDeque::new() };
    // Should the thread not start, its connection closes and takes `proxy` with it; sources then
    // ignore the app window's dangling `XdndProxy`, so winit's target is back.
    std::thread::Builder::new().name("photocraft-xdnd".into()).spawn(move || receiver.run()).map_err(|e| format!("cannot start the drop thread: {e}"))?;
    Ok(drops)
}

/// What a drag over the window brings that the app can open.
#[derive(Clone, Debug, PartialEq)]
enum Offer {
    /// Local files, by path.
    Files(Vec<PathBuf>),
    /// A file's contents, read on the drop in this type (named `mime`).
    Data(Atom, &'static str),
    /// A file the source saves where it's told, on the drop (Direct Save, in this type).
    DirectSave(Atom),
}

/// The drag over the window now.
struct Drag {
    source: Window,
    types: Vec<Atom>,
    /// Decided at its first position (`None` until then; `Some(None)`: nothing to open).
    offer: Option<Option<Offer>>,
}

struct Receiver {
    conn: RustConnection,
    atoms: Atoms,
    /// The window sources talk to.
    proxy: Window,
    drops: Arc<Mutex<Drops>>,
    wake: Box<dyn Fn() + Send>,
    drag: Option<Drag>,
    /// Events that arrived while a transfer was waited for, handled after it.
    pending: VecDeque<Event>,
}

impl Receiver {
    fn run(mut self) {
        loop {
            let event = match self.pending.pop_front().map(Ok).unwrap_or_else(|| self.conn.wait_for_event()) {
                Ok(event) => event,
                Err(e) => {
                    log::warn!("drag and drop: the X connection closed: {e}");
                    return;
                }
            };
            if let Event::ClientMessage(m) = event
                && let Err(e) = self.message(&m)
            {
                log::warn!("drag and drop: {e}");
            }
        }
    }

    fn message(&mut self, m: &ClientMessageEvent) -> Result<(), String> {
        let [source, flags, a, b, c] = m.data.as_data32();
        if m.type_ == self.atoms.XdndEnter {
            // Bit 0: more than three types, listed in `XdndTypeList` on the source's window.
            let types = if flags & 1 == 1 { self.type_list(source) } else { [a, b, c].into_iter().filter(|&t| t != x11rb::NONE).collect() };
            self.drag = Some(Drag { source, types, offer: None });
        } else if m.type_ == self.atoms.XdndPosition {
            return self.position(source, m.window, b);
        } else if m.type_ == self.atoms.XdndLeave {
            self.drag = None;
            self.set_hovered(Vec::new());
        } else if m.type_ == self.atoms.XdndDrop {
            return self.dropped(source, m.window, a);
        }
        Ok(())
    }

    /// The pointer moved over the window at `time`: decide what the drag would open, once, and
    /// answer `target` whether it's accepted.
    fn position(&mut self, source: Window, target: Window, time: u32) -> Result<(), String> {
        let undecided = self.drag.as_ref().filter(|d| d.source == source && d.offer.is_none()).map(|d| d.types.clone());
        if let Some(types) = undecided {
            let offer = self.offer(&types, time);
            self.set_hovered(hovered(offer.as_ref()));
            if let Some(drag) = self.drag.as_mut() {
                drag.offer = Some(offer);
            }
        }
        let accept = self.drag.as_ref().is_some_and(|d| d.source == source && matches!(d.offer, Some(Some(_))));
        let action = if accept { self.atoms.XdndActionCopy } else { x11rb::NONE };
        self.send(source, self.atoms.XdndStatus, [target, u32::from(accept), 0, 0, action])
    }

    /// What a drag of `types` opens: its local files, when its file list has any, else the
    /// first data type the app reads.
    fn offer(&mut self, types: &[Atom], time: u32) -> Option<Offer> {
        let named = self.names(types);
        let names: Vec<&str> = named.iter().map(|(_, n)| n.as_str()).collect();
        for mime in readable(&names) {
            let Some(&(atom, _)) = named.iter().find(|(_, n)| n == mime) else { continue };
            if mime == DIRECT_SAVE {
                return Some(Offer::DirectSave(atom));
            }
            if mime != URI_LIST {
                return Some(Offer::Data(atom, mime));
            }
            match self.fetch(atom, time, TRANSFER_TIMEOUT) {
                Ok(list) => {
                    let files = local_files(&list);
                    if !files.is_empty() {
                        return Some(Offer::Files(files));
                    }
                }
                Err(e) => log::warn!("drag and drop: couldn't read the dragged file list: {e}"),
            }
        }
        None
    }

    /// The drag was dropped on `target` at `time`: read what it brings, hand it to the UI and
    /// tell the source the drop is done (it may free its data then).
    fn dropped(&mut self, source: Window, target: Window, time: u32) -> Result<(), String> {
        let offer = self.drag.take().filter(|d| d.source == source).and_then(|d| d.offer.flatten());
        let (files, done): (Vec<egui::DroppedFileHandle>, bool) = match offer {
            None => (Vec::new(), false),
            Some(Offer::Files(paths)) => (paths.into_iter().map(|p| Arc::new(LocalFile(p)) as egui::DroppedFileHandle).collect(), true),
            Some(Offer::Data(atom, mime)) => {
                let name = data_name(self.direct_save_name(source).as_deref(), mime);
                data_drop(name, self.fetch(atom, time, TRANSFER_TIMEOUT))
            }
            Some(Offer::DirectSave(atom)) => {
                let name = data_name(self.direct_save_name(source).as_deref(), "");
                let bytes = self.direct_save(source, atom, &name, time);
                data_drop(name, bytes)
            }
        };
        {
            let mut drops = self.drops.lock().unwrap_or_else(PoisonError::into_inner);
            drops.hovered.clear();
            drops.dropped.extend(files);
        }
        (self.wake)();
        let action = if done { self.atoms.XdndActionCopy } else { x11rb::NONE };
        self.send(source, self.atoms.XdndFinished, [target, u32::from(done), action, 0, 0])
    }

    fn set_hovered(&self, hovered: Vec<egui::HoveredFile>) {
        self.drops.lock().unwrap_or_else(PoisonError::into_inner).hovered = hovered;
        (self.wake)();
    }

    fn send(&self, to: Window, kind: Atom, data: [u32; 5]) -> Result<(), String> {
        let event = ClientMessageEvent::new(32, to, kind, data);
        self.conn.send_event(false, to, EventMask::NO_EVENT, event).map_err(|e| e.to_string())?;
        self.conn.flush().map_err(|e| e.to_string())
    }

    /// The types a source with more than three lists on its window (empty when unreadable).
    fn type_list(&self, source: Window) -> Vec<Atom> {
        let reply = self.conn.get_property(false, source, self.atoms.XdndTypeList, AtomEnum::ATOM, 0, MAX_TYPES).ok().and_then(|c| c.reply().ok());
        reply.and_then(|r| r.value32().map(Iterator::collect)).unwrap_or_default()
    }

    /// `types` with their names (types whose name can't be read are left out).
    fn names(&self, types: &[Atom]) -> Vec<(Atom, String)> {
        let cookies: Vec<_> = types.iter().map(|&t| (t, self.conn.get_atom_name(t))).collect();
        cookies.into_iter().filter_map(|(t, cookie)| Some((t, String::from_utf8(cookie.ok()?.reply().ok()?.name).ok()?))).collect()
    }

    /// The file name the source proposes for its data (`XdndDirectSave0` on its window).
    fn direct_save_name(&self, source: Window) -> Option<String> {
        let reply = self.conn.get_property(false, source, self.atoms.XdndDirectSave0, AtomEnum::ANY, 0, 1024).ok()?.reply().ok()?;
        (reply.format == 8).then(|| String::from_utf8_lossy(&reply.value).into_owned())
    }

    /// Direct Save (XDS 0.1): the source saves its file `name` at the `file://` URI put in its
    /// `XdndDirectSave0` property, a private folder here, then answers the request for `atom`
    /// with "S"; "F" means it offers the bytes as `application/octet-stream` instead.
    fn direct_save(&mut self, source: Window, atom: Atom, name: &Path, time: u32) -> Result<Vec<u8>, String> {
        let dir = tempfile::Builder::new().prefix("photocraft-drop-").tempdir().map_err(|e| e.to_string())?;
        let path = dir.path().join(name);
        self.conn
            .change_property8(PropMode::REPLACE, source, self.atoms.XdndDirectSave0, self.atoms.TextPlain, file_uri(&path).as_bytes())
            .map_err(|e| e.to_string())?;
        let octet_stream = self.atoms.OctetStream;
        match self.fetch(atom, time, SAVE_TIMEOUT)?.as_slice() {
            b"S" => read_saved(&path),
            b"F" => self.fetch(octet_stream, time, TRANSFER_TIMEOUT),
            _ => Err("the app it was dragged from couldn't save the file".into()),
        }
    }

    /// The drag's data in type `target`, converted by its source at `time` within `timeout`
    /// (an ICCCM selection transfer; incremental when the data is large).
    fn fetch(&mut self, target: Atom, time: u32, timeout: Duration) -> Result<Vec<u8>, String> {
        let (proxy, selection, property) = (self.proxy, self.atoms.XdndSelection, self.atoms.PHOTOCRAFT_DROP);
        let end = Instant::now() + MAX_TRANSFER_TIME;
        let deadline = || (Instant::now() + timeout).min(end);
        self.conn.convert_selection(proxy, selection, target, property, time).map_err(|e| e.to_string())?;
        self.conn.flush().map_err(|e| e.to_string())?;
        let sent = self.wait(deadline(), |e| match e {
            Event::SelectionNotify(n) if n.requestor == proxy && n.selection == selection && n.target == target => Some(n.property),
            _ => None,
        })?;
        if sent == x11rb::NONE {
            return Err(format!("the source didn't send it as {}", self.name(target)));
        }
        let first = self.take_property(MAX_BYTES as usize)?;
        if first.type_ != self.atoms.INCR {
            return Ok(first.value);
        }
        // Taking the INCR property asked for the first part; each new value is the next part
        // and an empty one the end.
        let mut data = Vec::new();
        loop {
            self.wait(deadline(), |e| {
                matches!(e, Event::PropertyNotify(p) if p.window == proxy && p.atom == property && p.state == Property::NEW_VALUE).then_some(())
            })?;
            let part = self.take_property((MAX_BYTES as usize).saturating_sub(data.len()))?;
            if part.value.is_empty() {
                return Ok(data);
            }
            if data.len().saturating_add(part.value.len()) > MAX_BYTES as usize {
                return Err(too_large());
            }
            data.extend_from_slice(&part.value);
        }
    }

    /// Read and delete the transfer property on `proxy`, refusing one longer than `max_bytes`
    /// before the server sends it: an incremental part never takes more than the budget left.
    fn take_property(&self, max_bytes: usize) -> Result<GetPropertyReply, String> {
        let length = property_length(max_bytes);
        let cookie = self.conn.get_property(true, self.proxy, self.atoms.PHOTOCRAFT_DROP, AtomEnum::ANY, 0, length).map_err(|e| e.to_string())?;
        let reply = cookie.reply().map_err(|e| e.to_string())?;
        if reply.bytes_after > 0 {
            return Err(too_large());
        }
        Ok(reply)
    }

    /// The next event `pick` takes, before `deadline` however many others arrive; the others wait
    /// in `pending`, up to [`MAX_PENDING`].
    fn wait<T>(&mut self, deadline: Instant, pick: impl Fn(&Event) -> Option<T>) -> Result<T, String> {
        loop {
            if Instant::now() >= deadline {
                return Err("the app it was dragged from stopped answering".into());
            }
            match self.conn.poll_for_event().map_err(|e| e.to_string())? {
                Some(event) => match pick(&event) {
                    Some(picked) => return Ok(picked),
                    None if self.pending.len() >= MAX_PENDING => return Err("the app it was dragged from sent too many messages".into()),
                    None => self.pending.push_back(event),
                },
                None => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    }

    fn name(&self, atom: Atom) -> String {
        self.names(&[atom]).pop().map_or_else(|| atom.to_string(), |(_, n)| n)
    }
}

/// `GetProperty`'s length, in 32-bit units, for a read of at most `max_bytes`. It rounds up, so a
/// read may bring up to 3 bytes more; the incremental loop's count refuses those.
fn property_length(max_bytes: usize) -> u32 {
    u32::try_from(max_bytes.div_ceil(4)).unwrap_or(u32::MAX)
}

fn too_large() -> String {
    format!("the dragged data is larger than {} MB", MAX_BYTES >> 20)
}

/// What the UI gets for data dropped without a file: one file named `name`, and whether it was
/// read (the drop is then done).
fn data_drop(name: PathBuf, bytes: Result<Vec<u8>, String>) -> (Vec<egui::DroppedFileHandle>, bool) {
    let done = bytes.is_ok();
    (vec![Arc::new(DroppedData { name, bytes })], done)
}

/// A file a source saved for a Direct Save. The source made it, so it must be a plain file (not
/// a link, a pipe that would never end or a device), and no more than [`MAX_BYTES`] is read
/// however it grows.
fn read_saved(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let plain = |m: &std::fs::Metadata| if m.is_file() { Ok(m.len()) } else { Err("the app it was dragged from didn't save a plain file".to_string()) };
    let len = plain(&std::fs::symlink_metadata(path).map_err(|e| format!("the saved file is missing: {e}"))?)?;
    if len > u64::from(MAX_BYTES) {
        return Err(too_large());
    }
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    plain(&file.metadata().map_err(|e| e.to_string())?)?;
    let mut bytes = Vec::new();
    file.take(u64::from(MAX_BYTES) + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_BYTES as usize {
        return Err(too_large());
    }
    Ok(bytes)
}

/// `path` as a `file://` URI: every byte but unreserved characters and `/` percent-encoded.
fn file_uri(path: &Path) -> String {
    use std::fmt::Write as _;
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            uri.push(char::from(b));
        } else {
            let _ = write!(uri, "%{b:02X}");
        }
    }
    uri
}

/// The types named in `offered` the app reads, best first: a file list; the file of an image
/// dragged from a web page (`application/octet-stream` with `XdndDirectSave0`: Chromium's "save
/// it yourself" bytes); image data; a Direct Save.
fn readable(offered: &[&str]) -> Vec<&'static str> {
    let has = |t: &str| offered.contains(&t);
    let mut out = Vec::new();
    if has(URI_LIST) {
        out.push(URI_LIST);
    }
    if has(OCTET_STREAM) && has(DIRECT_SAVE) {
        out.push(OCTET_STREAM);
    }
    out.extend(IMAGE_TYPES.into_iter().filter(|t| has(t)));
    if has(DIRECT_SAVE) {
        out.push(DIRECT_SAVE);
    }
    out
}

/// The local files in a `text/uri-list` (RFC 2483: one URI per line, `#` comments). Lines may
/// end in CRLF or, from Chromium, LF alone. Other schemes are left out.
fn local_files(list: &[u8]) -> Vec<PathBuf> {
    String::from_utf8_lossy(list).lines().map(str::trim).filter(|l| !l.starts_with('#')).filter_map(crate::portal_save::file_uri_path).collect()
}

/// What the UI shows hovering for `offer`.
fn hovered(offer: Option<&Offer>) -> Vec<egui::HoveredFile> {
    match offer {
        Some(Offer::Files(paths)) => paths.iter().map(|p| egui::HoveredFile { path: Some(p.clone()), mime: String::new() }).collect(),
        Some(Offer::Data(_, mime)) => vec![egui::HoveredFile { path: None, mime: (*mime).to_string() }],
        Some(Offer::DirectSave(_)) => vec![egui::HoveredFile::default()],
        None => Vec::new(),
    }
}

/// The name dropped data opens under: the file name the source proposed (never a path, so it
/// can't point Open or Save at a file on disk), else Untitled with the data type's extension.
fn data_name(proposed: Option<&str>, mime: &str) -> PathBuf {
    let proposed = proposed.and_then(|p| Path::new(p.trim()).file_name()).map(|n| n.to_string_lossy().chars().filter(|c| !c.is_control()).collect::<String>());
    match (proposed.filter(|n| !n.is_empty()), mime.strip_prefix("image/")) {
        (Some(name), _) => PathBuf::from(name),
        (None, Some(ext)) => PathBuf::from(format!("Untitled.{ext}")),
        (None, None) => PathBuf::from("Untitled"),
    }
}

/// A file dropped by its path, read as winit's own drops are.
#[derive(Debug)]
struct LocalFile(PathBuf);

impl egui::DroppedFile for LocalFile {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// Data dropped without a file (an image from a web page): its file name, and its bytes or why
/// they couldn't be read.
struct DroppedData {
    name: PathBuf,
    bytes: Result<Vec<u8>, String>,
}

impl std::fmt::Debug for DroppedData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DroppedData").field("name", &self.name).field("bytes", &self.bytes.as_ref().map(Vec::len)).finish()
    }
}

impl egui::DroppedFile for DroppedData {
    fn path(&self) -> &Path {
        &self.name
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        self.bytes.clone()
    }
}

#[cfg(test)]
mod tests;
