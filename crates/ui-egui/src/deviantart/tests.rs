use super::*;

#[test]
fn a_rejected_refresh_token_is_recognized() {
    assert!(!super::errors::login_was_rejected("The refresh token is invalid"));
    assert!(super::errors::login_was_rejected("refresh-grant-rejected: invalid_grant"));
    assert!(!super::errors::login_was_rejected("invalid_token"));
    assert!(!super::errors::login_was_rejected("invalid_grant"));
    assert!(!super::errors::login_was_rejected("HTTP 500"));
}

#[test]
fn settings_drop_the_refresh_token() {
    let text = settings_json(&Settings { client_id: "99".into(), refresh_token: "secret-token".into(), title: "Hill".into(), ..Settings::default() });
    let (disk, token) = without_token(&text).unwrap();
    assert_eq!(token, "secret-token");
    assert!(!disk.contains("secret-token"));
    assert!(disk.contains("99"));
    let back = settings_from_json(&with_token(&disk, &token));
    assert_eq!(back.refresh_token, "secret-token");
    assert_eq!(back.title, "Hill");
}

#[test]
fn tags_keep_letters_numbers_and_underscores() {
    assert_eq!(clean_tags("night sky, night, café, a-b"), vec!["night".to_string(), "sky".to_string(), "caf".to_string(), "ab".to_string()]);
}

#[test]
fn submit_body_carries_the_file_and_not_a_token() {
    let (_, body) = multipart("Hill", "notes", &["night".into()], "hill.png", b"PNG");
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("name=\"title\""));
    assert!(text.contains("Hill"));
    assert!(text.contains("name=\"tags[0]\""));
    assert!(text.contains("filename=\"hill.png\""));
    assert!(text.contains("PNG"));
    assert!(!text.contains("refresh"));
    assert_eq!(item_id(r#"{"status":"success","itemid":42}"#), Some(42));
    assert_eq!(stash_url(2730074306229899).as_deref(), Some("https://www.deviantart.com/stash/0qvqatlhzez"));
    assert_eq!(stash_url(0), None);
    let (_, quoted) = multipart("Hill", "notes", &[], "my \"best\" sketch.png", b"x");
    let quoted = String::from_utf8_lossy(&quoted);
    assert!(quoted.contains("filename=\"my _best_ sketch.png\""));
    assert!(!quoted.contains("filename=\"my \"best\""));
}

#[test]
fn artist_comments_omit_the_local_path() {
    let text = comments("hill.png", 1200);
    assert!(text.contains("hill.png"));
    assert!(text.contains("PNG"));
    assert!(!text.contains('/'));
    assert!(!text.contains("home"));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn automatic_submissions_wait_for_the_active_job_and_run_in_order() {
    use std::sync::{Arc, Mutex, mpsc};
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.png");
    let second = dir.path().join("second.png");
    std::fs::write(&first, b"first").unwrap();
    std::fs::write(&second, b"second").unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sent = seen.clone();
    let mut app = PhotocraftApp::new(
        photocraft_engine::Session::new(),
        crate::Services {
            deviantart_submit: Some(Arc::new(move |_, _, _, _, _, filename, _| {
                sent.lock().unwrap().push(filename.to_string());
                Ok(r#"{"itemid":42}"#.into())
            })),
            save_deviantart: Some(Box::new(|_| Ok(()))),
            ..Default::default()
        },
    );
    app.deviantart = Settings { client_id: "42".into(), refresh_token: "token".into(), send_on_export: true, ..Default::default() };
    let (tx, rx) = mpsc::channel();
    app.deviantart_job = Some(Job { rx, kind: JobKind::Submit, path: String::new(), temporary: false, name: "active".into() });
    after_written(&mut app, &first.to_string_lossy(), WriteKind::Export);
    after_written(&mut app, &second.to_string_lossy(), WriteKind::Export);
    assert_eq!(app.deviantart_pending.len(), 2);
    assert!(seen.lock().unwrap().is_empty());
    tx.send(Ok("{}".into())).unwrap();
    let ctx = egui::Context::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while (app.deviantart_job.is_some() || !app.deviantart_pending.is_empty()) && std::time::Instant::now() < deadline {
        poll(&mut app, &ctx);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(app.deviantart_pending.is_empty());
    assert!(app.deviantart_job.is_none());
    assert_eq!(*seen.lock().unwrap(), vec!["first.png", "second.png"]);
}

#[test]
fn disconnect_drops_pending_automatic_submissions() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.deviantart_pending.push_back("private.png".into());
    errors::disconnect(&mut app);
    assert!(app.deviantart_pending.is_empty());
}

#[test]
fn multipart_boundary_avoids_file_and_text_delimiters() {
    let (content_type, _) = multipart("--PcDeviantArt0", "\r\n--PcDeviantArt1--\r\n", &[], "a.png", b"\r\n--PcDeviantArt2\r\n");
    assert_eq!(content_type, "multipart/form-data; boundary=PcDeviantArt3");
}

#[test]
fn access_token_error_does_not_discard_the_refresh_token() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.deviantart.refresh_token = "still-valid".into();
    errors::report_deviantart_error(&mut app, "invalid_token: access expired", false);
    assert_eq!(app.deviantart.refresh_token, "still-valid");
    errors::report_deviantart_error(&mut app, "refresh-grant-rejected: invalid_grant", false);
    assert!(app.deviantart.refresh_token.is_empty());
}

#[test]
fn disconnected_automatic_exports_keep_every_pending_path() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.deviantart.send_on_export = true;
    after_written(&mut app, "first.png", WriteKind::Export);
    after_written(&mut app, "second.png", WriteKind::Export);
    poll(&mut app, &egui::Context::default());
    assert_eq!(app.deviantart_pending.iter().map(String::as_str).collect::<Vec<_>>(), vec!["first.png", "second.png"]);
    assert_eq!(app.ui.dialogs.iter().filter(|dialog| owns(&dialog.fields)).count(), 1);
}

#[test]
fn window_close_waits_for_the_active_upload() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    let (_tx, rx) = std::sync::mpsc::channel();
    app.deviantart_job = Some(Job { rx, kind: JobKind::Submit, path: String::new(), temporary: false, name: String::new() });
    app.jobs.close_after_saves = true;
    let ctx = egui::Context::default();
    crate::jobs_ui::close_after_saves(&mut app, &ctx);
    assert!(app.jobs.close_after_saves);
    app.deviantart_job = None;
    crate::jobs_ui::close_after_saves(&mut app, &ctx);
    assert!(!app.jobs.close_after_saves);
}

#[test]
fn automation_cannot_confirm_a_denied_deviantart_dialog() {
    let services = crate::Services {
        automation_command: Some(Box::new(|id, _| {
            assert_eq!(id, "file.submitDeviantArt");
            Err("network denied".into())
        })),
        ..crate::Services::default()
    };
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
    let id = open_dialog(&mut app, None);
    app.automation_input = true;
    assert_eq!(crate::dialogs::confirm(&mut app, id), Err("network denied".into()));
    assert!(app.ui.dialog_mut(id).is_some());
}
