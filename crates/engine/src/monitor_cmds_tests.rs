//! Synthetic monitor setups and CC0 built-in profiles; no machine profiles or personal files.

use super::*;
use crate::display_color::Display;
use photocraft_color::{Color, ColorMode, SampleType};
use photocraft_doc::{Document, Size};

fn key(serial: u32) -> String {
    mac_display_key(0x1234, 0x5678, serial).unwrap()
}

fn display(id: u32, identity: Option<String>, profile: Option<Builtin>) -> Display {
    Display {
        id,
        key: identity,
        name: format!("Synthetic display {id}"),
        frame: [f64::from(id) * 100.0, 0.0, 100.0, 100.0],
        profile_name: profile.map(|p| p.profile().description.clone()),
        icc: profile.map(|p| p.profile().to_bytes()),
    }
}

fn choose(s: &mut Session, target: &str, profile: &str) {
    s.execute(COMMAND, json!({"set": [{"display": target, "profile": profile}]})).unwrap();
}

fn monitor_is(s: &Session, id: Option<u32>, expected: Builtin) {
    assert_eq!(s.color.monitor_for(id).content_hash(), expected.profile().content_hash());
}

fn doc(depth: SampleType) -> Document {
    let mut d = Document::with_background("Monitor test", Size::new(2, 2), ColorMode::Rgb, depth, Color::rgb(0.5, 0.25, 0.75));
    d.icc_profile = Some(Builtin::Srgb.profile().to_bytes());
    d
}

#[test]
fn physical_rules_survive_preferences_reordering_reconnection_and_runtime_id_reuse() {
    let (left, right) = (key(1), key(2));
    let mut s = Session::new();
    s.color.set_displays(Ok(vec![display(1, Some(left.clone()), Some(Builtin::Srgb)), display(2, Some(right.clone()), Some(Builtin::Srgb))]));
    s.execute(
        COMMAND,
        json!({"set": [
            {"display": "default", "profile": "srgb"},
            {"display": left, "profile": "display-p3"},
            {"display": right, "profile": "rec2020"}
        ]}),
    )
    .unwrap();
    monitor_is(&s, Some(1), Builtin::DisplayP3);
    monitor_is(&s, Some(2), Builtin::Rec2020);

    let saved = s.prefs_to_json();
    let mut restored = Session::new();
    restored.load_prefs_json(&saved).unwrap();
    restored.color.set_displays(Ok(vec![display(1, Some(right.clone()), None), display(17, Some(left.clone()), None)]));
    monitor_is(&restored, Some(1), Builtin::Rec2020);
    monitor_is(&restored, Some(17), Builtin::DisplayP3);
    restored.color.set_displays(Ok(vec![display(1, Some(right), None)]));
    monitor_is(&restored, Some(17), Builtin::Srgb);
    assert_eq!(restored.color.settings.monitor_overrides.get(&left).map(String::as_str), Some("display-p3"));
    restored.color.set_displays(Ok(vec![display(44, Some(left), None)]));
    monitor_is(&restored, Some(44), Builtin::DisplayP3);
}

#[test]
fn explicit_automatic_overrides_the_default_and_clear_restores_inheritance() {
    let left = key(1);
    let mut s = Session::new();
    s.color.set_displays(Ok(vec![display(1, Some(left.clone()), Some(Builtin::DisplayP3)), display(2, Some(key(2)), Some(Builtin::Srgb))]));
    choose(&mut s, DEFAULT_DISPLAY, "rec2020");
    choose(&mut s, &left, "auto");
    monitor_is(&s, Some(1), Builtin::DisplayP3);
    monitor_is(&s, Some(2), Builtin::Rec2020);
    assert_eq!(s.color.monitor_status_for(Some(1)).rule, left);
    s.execute(COMMAND, json!({"clear": [left]})).unwrap();
    monitor_is(&s, Some(1), Builtin::Rec2020);
    assert_eq!(s.color.monitor_status_for(Some(1)).rule, DEFAULT_DISPLAY);
}

#[test]
fn missing_ambiguous_or_stale_identity_cannot_inherit_a_physical_override() {
    let identity = key(1);
    let mut s = Session::new();
    s.color.set_displays(Ok(vec![display(1, Some(identity.clone()), None)]));
    choose(&mut s, DEFAULT_DISPLAY, "srgb");
    choose(&mut s, &identity, "display-p3");
    for displays in [vec![display(1, None, None)], vec![display(1, Some(identity.clone()), None), display(2, Some(identity.clone()), None)]] {
        s.color.set_displays(Ok(displays));
        monitor_is(&s, Some(1), Builtin::Srgb);
        assert!(s.execute(COMMAND, json!({"set": [{"display": identity, "profile": "rec2020"}]})).is_err());
        assert_eq!(s.color.settings.monitor_overrides.get(&identity).map(String::as_str), Some("display-p3"));
    }
    s.color.set_displays(Ok(vec![display(1, Some(identity.clone()), None)]));
    s.color.set_displays(Err("reader unavailable".into()));
    monitor_is(&s, Some(1), Builtin::Srgb);
    assert_eq!(s.color.monitor_status_for(Some(1)).identity, "stale");
    assert!(s.execute(COMMAND, json!({"set": [{"display": identity, "profile": "rec2020"}]})).is_err());

    for (vendor, serial) in [(0, 1), (UNKNOWN_VENDOR, 1), (u32::MAX, 1), (1, 0), (1, u32::MAX)] {
        assert!(mac_display_key(vendor, 1, serial).is_none());
    }
    assert_ne!(key(1), key(2));
}

#[test]
fn unknown_window_uses_default_not_the_main_windows_physical_rule_on_both_canvas_paths() {
    let identity = key(1);
    let mut s = Session::new();
    s.color.set_displays(Ok(vec![display(1, Some(identity.clone()), Some(Builtin::Srgb))]));
    s.color.main_display = Some(1);
    choose(&mut s, DEFAULT_DISPLAY, "srgb");
    choose(&mut s, &identity, "display-p3");
    for depth in [SampleType::U8, SampleType::U16, SampleType::F32] {
        let d = doc(depth);
        for target in [None, Some(99)] {
            monitor_is(&s, target, Builtin::Srgb);
            assert!(s.color.canvas_display_for(&d, target).unwrap().is_identity());
            assert!(s.color.gpu_canvas_lut_for(&d, 5, target).unwrap().is_none());
        }
        let canvas = s.color.canvas_display_for(&d, Some(1)).unwrap();
        assert!(!canvas.is_identity());
        monitor_is(&s, Some(1), Builtin::DisplayP3);
        assert_eq!(canvas.monitor.content_hash(), Builtin::DisplayP3.profile().content_hash());
        // The document source is parsed from ICC fixed-point fields; this independent
        // reference uses the built-in's unquantized model. Allow one RGB code value,
        // as in the CPU/GPU parity contract, while requiring exact destination and alpha.
        let assert_quantized = |actual: &[u8], reference: &[u8]| {
            assert_eq!(actual.len(), reference.len());
            for (i, (&a, &b)) in actual.iter().zip(reference).enumerate() {
                let tolerance = if i % 4 == 3 { 0 } else { 1 };
                assert!(a.abs_diff(b) <= tolerance, "byte {i}: {a} vs {b} ({depth:?})");
            }
        };
        let expected = Transform::new(Builtin::Srgb.profile(), Builtin::DisplayP3.profile(), DISPLAY_INTENT, DISPLAY_BPC).unwrap();
        let gpu = s.color.gpu_canvas_lut_for(&d, 5, Some(1)).unwrap().expect("the P3 destination needs a LUT");
        assert_quantized(&gpu, &photocraft_cms::Lut3d::from_transform(&expected, 5).to_rgba8());
        let raw = photocraft_compose::flatten(&d);
        let shown = canvas.to_rgba8(&raw);
        let mut reference = raw.to_rgba8();
        expected.convert_u8(&reference.pixels.clone(), 4, &mut reference.pixels, 4, true);
        assert_quantized(&shown.pixels, &reference.pixels);
    }
}

#[test]
fn invalid_batch_changes_nothing_and_monitor_commands_do_not_edit_documents() {
    let identity = key(1);
    let mut s = Session::new();
    s.add_document(doc(SampleType::U16), None);
    s.color.set_displays(Ok(vec![display(1, Some(identity.clone()), Some(Builtin::Srgb))]));
    let before = s.active().unwrap().clone();
    let settings = s.color.settings.clone();
    assert!(
        s.execute(
            COMMAND,
            json!({"set": [
                {"display": "default", "profile": "display-p3"},
                {"display": identity, "profile": "coated-cmyk"}
            ]})
        )
        .is_err()
    );
    assert_eq!(s.color.settings, settings);
    monitor_is(&s, Some(1), Builtin::Srgb);
    choose(&mut s, &identity, "display-p3");
    s.execute(COMMAND, json!({"refresh": true})).unwrap();
    s.execute(COMMAND, json!({"clear": [identity]})).unwrap();
    let after = s.active().unwrap();
    assert!(Arc::ptr_eq(&before.doc, &after.doc), "monitor settings must not replace the document, its ICC profile or pixels");
    assert_eq!((before.revision, before.is_dirty()), (after.revision, after.is_dirty()));
    assert_eq!((before.history.can_undo(), before.history.can_redo()), (after.history.can_undo(), after.history.can_redo()));
}

#[test]
fn malformed_monitor_requests_preserve_settings_and_runtime_revision() {
    let identity = key(1);
    let mut s = Session::new();
    s.color.set_displays(Ok(vec![display(1, Some(identity.clone()), Some(Builtin::Srgb))]));
    choose(&mut s, DEFAULT_DISPLAY, "rec2020");
    choose(&mut s, &identity, "display-p3");
    let settings = s.color.settings.clone();
    let revision = s.color.monitor_files.revision;
    let too_many_clears: Vec<_> = (0..=MAX_RULES).map(|n| format!("disconnected-{n}")).collect();
    let mut cases = vec![
        (json!({"set": [{"display": identity, "profile": "srgb"}, {"display": identity, "profile": "auto"}]}), "duplicate display change"),
        (json!({"clear": [identity, identity]}), "invalid or duplicate display key to clear"),
        (json!({"set": [{"display": identity, "profile": "srgb"}], "clear": [identity]}), "invalid or duplicate display key to clear"),
        (json!({"refresh": true, "set": [{"display": "default", "profile": "srgb"}]}), "refresh cannot be combined with changes"),
        (json!({"refresh": true, "clear": [identity]}), "refresh cannot be combined with changes"),
        (json!({"set": [{"display": "default", "profile": "srgb"}], "clear": too_many_clears}), "too many monitor changes"),
        (json!({"set": [{"display": "default", "profile": "srgb"}], "unexpected": true}), "unknown field"),
        (json!({"set": [{"display": "default", "profile": "srgb", "unexpected": true}]}), "unknown field"),
    ];
    for invalid in [String::new(), DEFAULT_DISPLAY.into(), "invalid\0key".into(), "x".repeat(MAX_KEY + 1)] {
        cases.push((json!({"clear": [identity, invalid]}), "invalid or duplicate display key to clear"));
    }
    for (params, expected) in cases {
        let error = s.execute(COMMAND, params.clone()).unwrap_err();
        assert!(matches!(&error, EngineError::BadParams { cmd, msg } if cmd == COMMAND && msg.contains(expected)), "{params}: {error}");
        assert_eq!(s.color.settings, settings, "{params}");
        assert_eq!(s.color.monitor_files.revision, revision, "{params}");
    }
}

#[test]
fn sixty_four_physical_rules_plus_default_are_allowed_but_an_extra_rule_changes_nothing() {
    let mut s = Session::new();
    let displays = (1..=MAX_RULES + 1).map(|n| {
        let id = u32::try_from(n).unwrap();
        display(id, Some(key(id)), Some(Builtin::Srgb))
    });
    s.color.set_displays(Ok(displays.collect()));
    let mut changes = vec![json!({"display": "default", "profile": "rec2020"})];
    for n in 1..=MAX_RULES {
        changes.push(json!({"display": key(u32::try_from(n).unwrap()), "profile": "display-p3"}));
    }
    s.execute(COMMAND, json!({"set": changes})).unwrap();
    assert_eq!(s.color.settings.monitor_overrides.len(), MAX_RULES);
    assert_eq!(s.color.settings.monitor_profile, "rec2020");
    let settings = s.color.settings.clone();
    let revision = s.color.monitor_files.revision;
    let extra = key(u32::try_from(MAX_RULES + 1).unwrap());
    let error = s.execute(COMMAND, json!({"set": [{"display": extra, "profile": "display-p3"}]})).unwrap_err();
    assert!(matches!(error, EngineError::BadParams { cmd, msg } if cmd == COMMAND && msg.contains("at most 64")));
    assert_eq!(s.color.settings, settings);
    assert_eq!(s.color.monitor_files.revision, revision);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn stale_and_cancelled_background_changes_cannot_overwrite_newer_choices() {
    use crate::jobs::Started;
    let mut s = Session::new();
    let Started::Job(old) = s.start(COMMAND, json!({"set": [{"display": "default", "profile": "display-p3"}]})).unwrap() else { panic!("native job") };
    choose(&mut s, DEFAULT_DISPLAY, "rec2020");
    assert!(s.wait_job(old).is_err());
    monitor_is(&s, None, Builtin::Rec2020);

    let Started::Job(old_refresh) = s.start(COMMAND, json!({"refresh": true})).unwrap() else { panic!("native job") };
    choose(&mut s, DEFAULT_DISPLAY, "srgb");
    assert_eq!(s.wait_job(old_refresh).unwrap()["stale"], true);
    monitor_is(&s, None, Builtin::Srgb);

    let Started::Job(cancelled) = s.start(COMMAND, json!({"set": [{"display": "default", "profile": "display-p3"}]})).unwrap() else { panic!("native job") };
    assert!(s.cancel_job(cancelled));
    assert!(matches!(s.wait_job(cancelled), Err(EngineError::Cancelled)));
    s.join_cancelled_jobs();
    monitor_is(&s, None, Builtin::Srgb);
}

#[test]
fn malformed_new_preferences_do_not_reset_working_spaces_or_legacy_default() {
    for damaged in [json!(false), json!(["wrong shape"]), json!({"broken": 2, "kept": "auto", "": "display-p3"})] {
        let mut s = Session::new();
        s.load_prefs_json(&json!({"colorSettings": {"workingRgb": "display-p3", "monitorProfile": "rec2020", "monitorOverrides": damaged}}).to_string())
            .unwrap();
        assert_eq!(s.color.settings.working_rgb, "display-p3");
        assert_eq!(s.color.settings.monitor_profile, "rec2020");
        assert!(!s.color.settings.monitor_overrides.contains_key("broken"));
        assert!(!s.color.settings.monitor_overrides.contains_key(""));
        monitor_is(&s, None, Builtin::Rec2020);
    }
    let mut legacy = Session::new();
    legacy.load_prefs_json(r#"{"colorSettings":{"monitorProfile":"display-p3"}}"#).unwrap();
    assert!(legacy.color.settings.monitor_overrides.is_empty());
    monitor_is(&legacy, None, Builtin::DisplayP3);
}

#[cfg(not(target_arch = "wasm32"))]
struct TempDir(std::path::PathBuf);

#[cfg(not(target_arch = "wasm32"))]
impl TempDir {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("photocraft-monitor-{}-{n}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn profile(&self) -> std::path::PathBuf {
        self.0.join("calibrated.icc")
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn lenient_preference_loading_preserves_monitor_choices_and_invalidates_cached_files() {
    let dir = TempDir::new();
    let path = dir.profile();
    let spec = path.to_str().unwrap();
    let identity = key(1);
    std::fs::write(&path, Builtin::Rec2020.profile().to_bytes().as_slice()).unwrap();
    let mut s = Session::new();
    s.color.set_displays(Ok(vec![display(1, Some(identity.clone()), Some(Builtin::Srgb))]));
    choose(&mut s, DEFAULT_DISPLAY, "rec2020");
    choose(&mut s, &identity, spec);
    let original_fingerprint = s.color.monitor_status_for(Some(1)).fingerprint;
    let revision = s.color.monitor_files.revision;
    assert!(s.color.monitor_files.files.contains_key(spec));

    let mut saved: Value = serde_json::from_str(&s.prefs_to_json()).unwrap();
    saved["colorSettings"]["workingRgb"] = json!("display-p3");
    saved["colorSettings"]["bpc"] = json!("yes");
    saved["general"]["exportClipboard"] = json!(false);
    saved["general"]["beepWhenDone"] = json!("yes");
    let mut expected_color = s.color.settings.clone();
    expected_color.working_rgb = "display-p3".into();
    expected_color.bpc = ColorSettings::default().bpc;
    std::fs::write(&path, Builtin::DisplayP3.profile().to_bytes().as_slice()).unwrap();

    let invalid = s.load_prefs_json(&saved.to_string()).unwrap();
    let invalid_paths: BTreeSet<_> = invalid.iter().map(crate::prefs::InvalidPref::path).collect();
    assert_eq!(invalid_paths, BTreeSet::from(["colorSettings.bpc".into(), "general.beepWhenDone".into()]));
    assert_eq!(s.color.settings, expected_color, "valid default and physical monitor choices survive unrelated malformed fields");
    assert!(!s.prefs().general.export_clipboard);
    assert_eq!(s.prefs().general.beep_when_done, crate::prefs::General::default().beep_when_done);
    assert!(s.color.monitor_files.files.is_empty());
    assert_ne!(s.color.monitor_files.revision, revision);
    assert!(s.color.monitor_status_for(Some(1)).loading);
    monitor_is(&s, None, Builtin::Rec2020);
    monitor_is(&s, Some(1), Builtin::Srgb);

    s.execute(COMMAND, json!({"refresh": true})).unwrap();
    let refreshed = s.color.monitor_status_for(Some(1));
    assert!(!refreshed.loading);
    assert_ne!(refreshed.fingerprint, original_fingerprint);
    assert_eq!(s.color.monitor_for(Some(1)).description, Builtin::DisplayP3.profile().description);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn refresh_observes_replacement_even_with_identical_path_length_and_timestamp() {
    let dir = TempDir::new();
    let path = dir.profile();
    let mut a = Builtin::Srgb.profile().to_bytes().to_vec();
    let mut b = Builtin::DisplayP3.profile().to_bytes().to_vec();
    let size = a.len().max(b.len());
    // ICC allows trailing padding. Make the two complete profiles have identical file sizes.
    for bytes in [&mut a, &mut b] {
        bytes.resize(size, 0);
        bytes[..4].copy_from_slice(&(size as u32).to_be_bytes());
    }
    let modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    let write = |bytes: &[u8]| {
        std::fs::write(&path, bytes).unwrap();
        std::fs::OpenOptions::new().write(true).open(&path).unwrap().set_times(std::fs::FileTimes::new().set_modified(modified)).unwrap();
    };
    write(&a);
    let original_metadata = std::fs::metadata(&path).unwrap();
    let mut s = Session::new();
    choose(&mut s, DEFAULT_DISPLAY, path.to_str().unwrap());
    let d = doc(SampleType::U8);
    let original_status = s.color.monitor_status();
    let original_canvas = s.color.canvas_display(&d).unwrap();
    write(&b);
    let replacement_metadata = std::fs::metadata(&path).unwrap();
    assert_eq!(original_metadata.len(), replacement_metadata.len());
    assert_eq!(original_metadata.modified().unwrap(), replacement_metadata.modified().unwrap());
    assert_eq!(s.color.monitor_status().fingerprint, original_status.fingerprint, "frame resolution must only read the cache");
    s.execute(COMMAND, json!({"refresh": true})).unwrap();
    assert_ne!(s.color.monitor_status().fingerprint, original_status.fingerprint);
    assert_ne!(s.color.canvas_display(&d).unwrap().key, original_canvas.key);
    assert_eq!(s.color.monitor().description, Builtin::DisplayP3.profile().description);
    assert_eq!(s.color.monitor_files.files.get(path.to_str().unwrap()).unwrap().as_ref().unwrap().fingerprint, fingerprint(&b));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn invalid_or_deleted_manual_files_fall_back_without_losing_the_choice_and_can_recover() {
    let dir = TempDir::new();
    let path = dir.profile();
    let spec = path.to_str().unwrap();
    let identity = key(1);
    std::fs::write(&path, Builtin::Srgb.profile().to_bytes().as_slice()).unwrap();
    let mut s = Session::new();
    s.color.set_displays(Ok(vec![display(1, Some(identity.clone()), Some(Builtin::DisplayP3))]));
    choose(&mut s, &identity, spec);
    let mut link = Builtin::Srgb.profile().clone();
    link.class = ProfileClass::DeviceLink;
    let link = link.with_encoded_bytes();
    for bytes in [b"not an ICC profile".to_vec(), Builtin::CoatedCmyk.profile().to_bytes().to_vec(), link.to_bytes().to_vec()] {
        std::fs::write(&path, bytes).unwrap();
        s.execute(COMMAND, json!({"refresh": true})).unwrap();
        let status = s.color.monitor_status_for(Some(1));
        assert_eq!(status.source, "auto");
        assert!(!status.loading && status.reason.is_some());
        assert_eq!(status.requested, spec);
        monitor_is(&s, Some(1), Builtin::DisplayP3);
        assert_eq!(s.color.settings.monitor_overrides.get(&identity).map(String::as_str), Some(spec));
    }
    std::fs::remove_file(&path).unwrap();
    s.execute(COMMAND, json!({"refresh": true})).unwrap();
    monitor_is(&s, Some(1), Builtin::DisplayP3);
    s.color.set_displays(Ok(vec![display(1, Some(identity.clone()), None)]));
    let status = s.color.monitor_status_for(Some(1));
    assert_eq!(status.source, "fallback");
    assert!(status.reason.as_deref().unwrap().contains("cannot read profile"));
    monitor_is(&s, Some(1), Builtin::Srgb);
    std::fs::write(&path, Builtin::Rec2020.profile().to_bytes().as_slice()).unwrap();
    s.execute(COMMAND, json!({"refresh": true})).unwrap();
    monitor_is(&s, Some(1), Builtin::Rec2020);
    assert_eq!(s.color.monitor_status_for(Some(1)).source, "manual");
    assert!(s.color.monitor_status_for(Some(1)).reason.is_none());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn first_file_read_is_loading_and_deleted_monitor_file_does_not_block_working_space_edits() {
    let dir = TempDir::new();
    let path = dir.profile();
    let spec = path.to_str().unwrap();
    let mut s = Session::new();
    s.load_prefs_json(&json!({"colorSettings": {"monitorProfile": spec}}).to_string()).unwrap();
    s.color.set_displays(Ok(vec![display(1, Some(key(1)), Some(Builtin::DisplayP3))]));
    let loading = s.color.monitor_status_for(Some(1));
    assert!(loading.loading && loading.reason.is_none());
    monitor_is(&s, Some(1), Builtin::DisplayP3);
    s.execute(COMMAND, json!({"refresh": true})).unwrap();
    let failed = s.color.monitor_status_for(Some(1));
    assert!(!failed.loading && failed.reason.is_some());
    s.execute("edit.colorSettings", json!({"workingRgb": "display-p3"})).unwrap();
    assert_eq!(s.color.settings.working_rgb, "display-p3");
    s.execute("prefs.set", json!({"path": "colorSettings.workingRgb", "value": "rec2020"})).unwrap();
    assert_eq!(s.color.settings.working_rgb, "rec2020");
    assert_eq!(s.color.settings.monitor_profile, spec);
}

#[cfg(all(not(target_arch = "wasm32"), unix))]
#[test]
fn profile_reader_rejects_fifo_device_directory_and_oversized_file_without_blocking() {
    let dir = TempDir::new();
    let fifo = dir.0.join("pipe.icc");
    assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_profile_bytes(&fifo));
    });
    assert!(rx.recv_timeout(std::time::Duration::from_secs(2)).expect("FIFO must be rejected before open").is_err());
    assert!(read_profile_bytes(Path::new("/dev/null")).is_err());
    assert!(read_profile_bytes(&dir.0).is_err());
    let huge = dir.0.join("too-big.icc");
    std::fs::File::create(&huge).unwrap().set_len(MAX_PROFILE_BYTES + 1).unwrap();
    assert!(read_profile_bytes(&huge).is_err());
}
