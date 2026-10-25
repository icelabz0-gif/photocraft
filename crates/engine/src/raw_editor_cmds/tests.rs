use super::*;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("photocraft-raw-editor-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn with_editor(path: &str) -> Session {
    let mut s = Session::new();
    let editor = path.to_string();
    s.edit_prefs(|p| p.raw_defaults.external_editor = editor);
    s
}

#[test]
fn only_darktable_and_rapidraw_are_editors() {
    let dt = editor_paths("/usr/bin/darktable").unwrap();
    assert_eq!((dt.editor, dt.cli), (Editor::Darktable, PathBuf::from("/usr/bin/darktable-cli")));
    let cli = editor_paths("/opt/dt/bin/darktable-cli").unwrap();
    assert_eq!((cli.gui, cli.cli), (PathBuf::from("/opt/dt/bin/darktable"), PathBuf::from("/opt/dt/bin/darktable-cli")));
    let win = editor_paths("/opt/darktable/bin/Darktable.exe").unwrap();
    assert!(win.cli.to_string_lossy().ends_with("bin/darktable-cli.exe"), "{:?}", win.cli);
    let mac = editor_paths("/Applications/darktable.app").unwrap();
    assert_eq!(mac.cli, PathBuf::from("/Applications/darktable.app/Contents/MacOS/darktable-cli"));
    let rr = editor_paths("/usr/bin/rapidraw").unwrap();
    assert_eq!((rr.editor, &rr.gui, &rr.cli), (Editor::RapidRaw, &rr.cli, &PathBuf::from("/usr/bin/rapidraw")));
    assert_eq!(editor_paths("/Applications/RapidRAW.app").unwrap().cli, PathBuf::from("/Applications/RapidRAW.app/Contents/MacOS/RapidRAW"));
    // Any other program is refused, as is nothing at all.
    for p in ["", "  ", "/bin/sh", "/usr/bin/darktable-evil", "darktable.sh", "/tmp/rapidraw/x"] {
        assert!(editor_paths(p).is_err(), "{p}");
    }
}

#[test]
fn commands_follow_each_editors_manual() {
    let dt = editor_paths("/usr/bin/darktable").unwrap();
    assert_eq!(edit_command(&dt, "/p/a.cr2"), ("/usr/bin/darktable".into(), vec!["/p/a.cr2".to_string()]));
    let (program, args) = develop_command(&dt, "/p/a.cr2", "/t/a.tif");
    assert_eq!(program, "/usr/bin/darktable-cli");
    assert_eq!(args, ["/p/a.cr2", "/t/a.tif", "--apply-custom-presets", "false", "--core", "--conf", "plugins/imageio/format/tiff/bpp=16"]);
    let rr = editor_paths("/usr/bin/RapidRAW").unwrap();
    let (program, args) = develop_command(&rr, "/p/a.cr2", "/t/a.tif");
    assert_eq!(program, "/usr/bin/RapidRAW");
    assert_eq!(args, ["export", "/p/a.cr2", "--output", "/t/a.tif", "--format", "tiff", "--tiff-bit-depth", "16"]);
    let mac = editor_paths("/Applications/RapidRAW.app").unwrap();
    assert_eq!(edit_command(&mac, "/p/a.nef"), ("open".into(), vec!["-a".to_string(), "/Applications/RapidRAW.app".into(), "/p/a.nef".into()]));
}

#[test]
fn bad_params_and_settings_are_errors() {
    let mut s = Session::new();
    // No editor set.
    assert!(s.execute(EDIT, json!({"path": "/p/a.cr2", "dryRun": true})).is_err());
    let mut s2 = with_editor("/bin/sh");
    assert!(s2.execute(DEVELOP, json!({"path": "/p/a.cr2", "dryRun": true})).is_err());
    let mut s = with_editor("/usr/bin/darktable");
    for p in [json!({}), json!({"path": ""}), json!({"path": 3}), json!({"path": "/p/a.png"}), json!({"path": "/p/noext"})] {
        assert!(s.execute(EDIT, p.clone()).is_err(), "{p}");
        assert!(s.execute(DEVELOP, p.clone()).is_err(), "{p}");
    }
    // A raw that isn't there.
    assert!(s.execute(EDIT, json!({"path": "/no/such/a.cr2"})).is_err());
    assert!(s.execute(DEVELOP, json!({"path": "/no/such/a.cr2"})).is_err());
}

#[test]
fn a_relative_raw_is_made_absolute() {
    let mut s = with_editor("/usr/bin/darktable");
    let r = s.execute(EDIT, json!({"path": "-shoot.cr2", "dryRun": true})).unwrap();
    let raw = r["args"][0].as_str().unwrap();
    assert!(std::path::Path::new(raw).is_absolute() && !raw.starts_with('-'), "{raw}");
}

/// A stand-in `darktable-cli` writes a TIFF where it is told; Develop opens it as a new, untitled
/// document named after the raw.
#[cfg(unix)]
#[test]
fn develop_opens_what_the_editor_writes() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmp("develop");
    let tif = dir.join("made.tif");
    let mut src = Session::new();
    src.execute("file.new", json!({"width": 6, "height": 4, "background": "#336699"})).unwrap();
    crate::file_cmds::save_doc(&src.active().unwrap().doc, &tif.to_string_lossy(), None).unwrap();
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // Arguments: raw, output, options. Records them, then writes the output.
    let script = format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\ncp '{}' \"$2\"\n", dir.join("args.txt").display(), tif.display());
    let cli = bin.join("darktable-cli");
    std::fs::write(&cli, script).unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    let raw = dir.join("IMG_0001.CR2");
    std::fs::write(&raw, b"not really a raw").unwrap();
    let mut s = with_editor(&bin.join("darktable").to_string_lossy());
    let r = s.execute(DEVELOP, json!({"path": raw.to_string_lossy()})).unwrap();
    assert_eq!(r["name"], "IMG_0001.tif");
    let d = s.active().unwrap();
    assert_eq!((d.doc.size.width, d.doc.size.height), (6, 4));
    assert_eq!(d.doc.name, "IMG_0001.tif");
    assert_eq!(d.path, None, "a save asks where, not into the scratch folder");
    let args = std::fs::read_to_string(dir.join("args.txt")).unwrap();
    assert!(args.starts_with(&format!("{}\n", raw.display())), "{args}");
    assert!(args.contains("plugins/imageio/format/tiff/bpp=16"), "{args}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// An editor that fails, or writes nothing, is an error naming it, and opens nothing.
#[cfg(unix)]
#[test]
fn a_failed_develop_opens_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmp("fail");
    let raw = dir.join("a.nef");
    std::fs::write(&raw, b"x").unwrap();
    for (body, want) in [("echo 'cannot read file' >&2\nexit 3", "cannot read file"), ("exit 0", "wrote no image")] {
        let cli = dir.join("RapidRAW");
        std::fs::write(&cli, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut s = with_editor(&cli.to_string_lossy());
        let e = s.execute(DEVELOP, json!({"path": raw.to_string_lossy()})).unwrap_err().to_string();
        assert!(e.contains(want), "{e}");
        assert!(s.documents().is_empty());
    }
    let _ = std::fs::remove_dir_all(&dir);
}
