//! File › Edit in Raw Editor and File › Develop in Raw Editor: a round trip through darktable or
//! RapidRAW, the free raw editors, as separate programs (Preferences › Camera Raw › External
//! Editor).
//!
//! Edit opens the raw file in the editor's window. Develop runs the editor's documented headless
//! export (`darktable-cli`, `RapidRAW export`), which applies the edits saved beside the raw (an
//! `.xmp` sidecar for darktable, `.rrdata` for RapidRAW), and opens the 16-bit TIFF it writes as a
//! new, untitled document. Nothing of either editor is linked into PhotoCraft: they run as their
//! own processes, so their licences (GPL, AGPL) stay theirs.
//!
//! Only a program named after one of the two editors runs: the path is a preference an agent can
//! set, and it must not become a way to start any program.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

const EDIT: &str = "file.editInRawEditor";
const DEVELOP: &str = "file.developInRawEditor";

/// The raw editors PhotoCraft can hand a raw file to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Editor {
    Darktable,
    RapidRaw,
}

impl Editor {
    fn name(self) -> &'static str {
        match self {
            Editor::Darktable => "darktable",
            Editor::RapidRaw => "RapidRAW",
        }
    }
}

/// How to run a configured editor: its window, and its headless export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorPaths {
    pub editor: Editor,
    /// The program showing the editor's window, or the `.app` bundle on macOS.
    pub gui: PathBuf,
    /// The headless exporter: `darktable-cli`, or RapidRAW itself.
    pub cli: PathBuf,
}

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

/// What `path` (Preferences › Camera Raw › External Editor) runs: `darktable`, `darktable-cli`,
/// `RapidRAW` (with `.exe` on Windows) or their `.app` bundles, matched without case. Anything else
/// is refused.
pub fn editor_paths(path: &str) -> std::result::Result<EditorPaths, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("no raw editor is set: choose darktable or RapidRAW in Preferences › Camera Raw › External Editor".into());
    }
    let p = Path::new(path);
    let file = p.file_name().map(|f| f.to_string_lossy().to_lowercase()).unwrap_or_default();
    let (stem, bundle) = match file.strip_suffix(".app") {
        Some(stem) => (stem.to_string(), true),
        None => (file.strip_suffix(".exe").unwrap_or(&file).to_string(), false),
    };
    let exe = |name: &str| if file.ends_with(".exe") { format!("{name}.exe") } else { name.to_string() };
    let sibling = |name: &str| p.with_file_name(exe(name));
    let in_bundle = |name: &str| p.join("Contents").join("MacOS").join(name);
    match (stem.as_str(), bundle) {
        ("darktable", false) => Ok(EditorPaths { editor: Editor::Darktable, gui: p.into(), cli: sibling("darktable-cli") }),
        ("darktable-cli", false) => Ok(EditorPaths { editor: Editor::Darktable, gui: sibling("darktable"), cli: p.into() }),
        ("darktable", true) => Ok(EditorPaths { editor: Editor::Darktable, gui: p.into(), cli: in_bundle("darktable-cli") }),
        ("rapidraw", false) => Ok(EditorPaths { editor: Editor::RapidRaw, gui: p.into(), cli: p.into() }),
        ("rapidraw", true) => Ok(EditorPaths { editor: Editor::RapidRaw, gui: p.into(), cli: in_bundle("RapidRAW") }),
        _ => Err(format!("{path} is not darktable or RapidRAW: External Editor runs only those two raw editors")),
    }
}

/// The program and arguments that open `raw` in the editor's window. A macOS bundle opens through
/// `open -a`.
pub fn edit_command(e: &EditorPaths, raw: &str) -> (String, Vec<String>) {
    let gui = e.gui.to_string_lossy().into_owned();
    if gui.to_lowercase().ends_with(".app") { ("open".into(), vec!["-a".into(), gui, raw.into()]) } else { (gui, vec![raw.into()]) }
}

/// The program and arguments that develop `raw` into the 16-bit TIFF `out`, with the edits saved
/// beside the raw. darktable finds the `.xmp` sidecar itself; `--apply-custom-presets false` lets
/// it run while darktable's window is open (its manual page), and its bit depth is set through
/// `--core --conf`, as the manual says `--bpp` does nothing yet. RapidRAW's `export` reads the
/// `.rrdata` sidecar.
pub fn develop_command(e: &EditorPaths, raw: &str, out: &str) -> (String, Vec<String>) {
    let cli = e.cli.to_string_lossy().into_owned();
    let args: Vec<&str> = match e.editor {
        Editor::Darktable => vec![raw, out, "--apply-custom-presets", "false", "--core", "--conf", "plugins/imageio/format/tiff/bpp=16"],
        Editor::RapidRaw => vec!["export", raw, "--output", out, "--format", "tiff", "--tiff-bit-depth", "16"],
    };
    (cli, args.into_iter().map(String::from).collect())
}

/// Whether `path` names a raw file PhotoCraft knows, by its extension.
fn is_raw_path(path: &str) -> bool {
    crate::file_cmds::extension(path).is_some_and(|ext| photocraft_io::raw::EXTENSIONS.contains(&ext.as_str()))
}

/// The raw file a command works on: `path`, else the active document's file when it is a raw. The
/// result is absolute, so it never starts with `-` and reads as an option.
fn raw_param(s: &Session, p: &Value, cmd: &str) -> Result<String> {
    let path = match p.get("path") {
        Some(Value::String(v)) if !v.trim().is_empty() => v.clone(),
        Some(Value::String(_)) => return Err(bad(cmd, "`path` is empty")),
        Some(Value::Null) | None => {
            let d = s.active().ok_or_else(|| bad(cmd, "give `path` (a raw file), or open a raw file first"))?;
            d.path.clone().filter(|p| is_raw_path(p)).ok_or_else(|| bad(cmd, "the active document is not a raw file: give `path`"))?
        }
        Some(_) => return Err(bad(cmd, "`path` must be a string")),
    };
    if !is_raw_path(&path) {
        return Err(bad(cmd, format!("{path} is not a raw file ({})", photocraft_io::raw::EXTENSIONS.join(", "))));
    }
    let abs = std::path::absolute(&path).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    Ok(abs.to_string_lossy().into_owned())
}

fn configured(s: &Session, cmd: &str) -> Result<EditorPaths> {
    editor_paths(&s.prefs().raw_defaults.external_editor).map_err(|e| bad(cmd, e))
}

fn dry_run(p: &Value) -> bool {
    p.get("dryRun").and_then(Value::as_bool).unwrap_or(false)
}

fn edit(s: &mut Session, p: &Value) -> Result<Value> {
    let e = configured(s, EDIT)?;
    let raw = raw_param(s, p, EDIT)?;
    let (program, args) = edit_command(&e, &raw);
    if dry_run(p) {
        return Ok(json!({"editor": e.editor.name(), "program": program, "args": args}));
    }
    if !Path::new(&raw).is_file() {
        return Err(EngineError::Other(format!("{raw}: no such file")));
    }
    spawn(&program, &args)?;
    Ok(json!({"editor": e.editor.name(), "path": raw}))
}

fn develop(s: &mut Session, p: &Value) -> Result<Value> {
    let e = configured(s, DEVELOP)?;
    let raw = raw_param(s, p, DEVELOP)?;
    let stem = crate::file_cmds::stem(&raw);
    if dry_run(p) {
        let (program, args) = develop_command(&e, &raw, &format!("{stem}.tif"));
        return Ok(json!({"editor": e.editor.name(), "program": program, "args": args}));
    }
    if !Path::new(&raw).is_file() {
        return Err(EngineError::Other(format!("{raw}: no such file")));
    }
    let dir = scratch_dir()?;
    let out = dir.join(format!("{}.tif", crate::file_cmds::sanitize(&stem)));
    let out_s = out.to_string_lossy().into_owned();
    let (program, args) = develop_command(&e, &raw, &out_s);
    let developed = run(&program, &args).and_then(|()| {
        let bytes = crate::file_cmds::read_file(&out_s).map_err(|_| EngineError::Other(format!("{} wrote no image ({out_s} is missing)", e.editor.name())))?;
        crate::file_cmds::import_opened(s, &format!("{stem}.tif"), &bytes)
    });
    let _ = std::fs::remove_dir_all(&dir);
    let mut doc = developed?;
    doc.name = format!("{stem}.tif");
    // Untitled: Save asks where, so nothing is written into the scratch folder or over the raw.
    let (index, report) = s.open_document(doc, None);
    Ok(json!({"editor": e.editor.name(), "document": index, "name": format!("{stem}.tif"), "profile": report}))
}

/// A new, empty folder for one develop's output.
#[cfg(not(target_arch = "wasm32"))]
fn scratch_dir() -> Result<PathBuf> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("photocraft-raw-editor-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| EngineError::Other(format!("{}: {e}", dir.display())))?;
    Ok(dir)
}

#[cfg(target_arch = "wasm32")]
fn scratch_dir() -> Result<PathBuf> {
    Err(EngineError::Other("a raw editor needs the desktop app".into()))
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn(program: &str, args: &[String]) -> Result<()> {
    std::process::Command::new(program).args(args).spawn().map(|_| ()).map_err(|e| EngineError::Other(format!("{program}: {e}")))
}

/// Runs `program` to the end; a failure carries the end of what it printed.
#[cfg(not(target_arch = "wasm32"))]
fn run(program: &str, args: &[String]) -> Result<()> {
    let out = std::process::Command::new(program).args(args).output().map_err(|e| EngineError::Other(format!("{program}: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(if out.stderr.is_empty() { &out.stdout } else { &out.stderr }).trim().to_string();
    let tail: String = text.chars().rev().take(400).collect::<Vec<_>>().into_iter().rev().collect();
    Err(EngineError::Other(format!("{program} failed ({}): {tail}", out.status)))
}

#[cfg(target_arch = "wasm32")]
fn spawn(_: &str, _: &[String]) -> Result<()> {
    Err(EngineError::Other("a raw editor needs the desktop app".into()))
}

#[cfg(target_arch = "wasm32")]
fn run(_: &str, _: &[String]) -> Result<()> {
    Err(EngineError::Other("a raw editor needs the desktop app".into()))
}

fn native(_: &Session) -> std::result::Result<(), String> {
    if cfg!(target_arch = "wasm32") { Err("not available in the browser".into()) } else { Ok(()) }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: EDIT,
            label: "Edit in Raw Editor",
            menu: &["File"],
            shortcut: None,
            params: r#"{"path":string?,"dryRun":bool=false} → {editor, path} (or {editor, program, args} with dryRun): open a raw file (default: the active document's) in darktable or RapidRAW, as set in Preferences › Camera Raw › External Editor"#,
            enabled: native,
            journal: false,
            run: edit,
        },
        CommandSpec {
            id: DEVELOP,
            label: "Develop in Raw Editor",
            menu: &["File"],
            shortcut: None,
            params: r#"{"path":string?,"dryRun":bool=false} → {editor, document, name} (or {editor, program, args} with dryRun): develop a raw file (default: the active document's) with darktable-cli or RapidRAW's export, applying the edits saved beside it (.xmp, .rrdata), and open the 16-bit TIFF as a new document"#,
            enabled: native,
            journal: true,
            run: develop,
        },
    ]
}

#[cfg(test)]
#[path = "raw_editor_cmds/tests.rs"]
mod tests;
