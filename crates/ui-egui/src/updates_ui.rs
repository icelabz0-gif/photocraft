//! One background check at startup and a non-modal window for release information.

use photocraft_engine::updates_cmds::Status;
use serde_json::Value;

use crate::PhotocraftApp;

#[derive(Default)]
pub(crate) struct Runtime {
    startup_checked: bool,
    notified_version: Option<String>,
    #[cfg(not(target_arch = "wasm32"))]
    prompt: Option<craft_update::StartupPrompt>,
    #[cfg(not(target_arch = "wasm32"))]
    update_rx: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
}

pub(crate) fn check(app: &mut PhotocraftApp, _ctx: &egui::Context, params: Value) -> Result<Value, String> {
    let before = app.session.updates.status.clone();
    let result = app.run("help.checkForUpdates", params);
    // A check that fails to start still shows its failure and Check Again; the command error
    // stands. Only this call's failure: a Failed left from an earlier check doesn't open the
    // window for a call refused for another reason (bad params).
    let failed_to_start = match (&result, &app.session.updates.status) {
        (Err(error), status @ Status::Failed { message }) => *status != before || (!message.is_empty() && error.contains(message.as_str())),
        _ => false,
    };
    if result.is_ok() || failed_to_start {
        app.ui.updates_open = true;
        app.updates_rt.startup_checked = true;
    }
    result
}

pub(crate) fn tick(app: &mut PhotocraftApp, ctx: &egui::Context) {
    // `updates.status` also polls (automation may consume the result before this frame).
    app.session.updates.poll();
    if let Status::Available { release } = &app.session.updates.status
        && app.updates_rt.notified_version.as_ref() != Some(&release.version)
    {
        app.updates_rt.notified_version = Some(release.version.clone());
        app.ui.updates_open = true;
        #[cfg(not(target_arch = "wasm32"))]
        {
            let update_rel = craft_update::Release {
                tag: format!("v{}", release.version),
                version: craft_update::parse_version(&release.version),
                name: format!("PhotoCraft {}", release.version),
                html_url: release.url.clone(),
                ..Default::default()
            };
            app.updates_rt.prompt =
                Some(craft_update::StartupPrompt::new(&craft_update::suite::PHOTOCRAFT, update_rel, photocraft_engine::build_info::VERSION));
        }
    }
    if app.session.updates.check.is_none() {
        return;
    }
    if !app.updates_rt.startup_checked {
        app.updates_rt.startup_checked = true;
        if app.session.prefs().general.automatically_check_updates && matches!(app.session.updates.status, Status::Idle) {
            let _ = app.run("help.checkForUpdates", serde_json::json!({}));
        }
    }
    if matches!(app.session.updates.status, Status::Checking) {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

pub(crate) fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let mut download: Option<String> = None;

    #[cfg(not(target_arch = "wasm32"))]
    if let Some(prompt) = &mut app.updates_rt.prompt {
        if let Some(result) = app.updates_rt.update_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            app.updates_rt.update_rx = None;
            match result {
                // The guarded close path asks about unsaved work and waits for saves; the restart helper
                // relaunches once the process exits.
                Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(e) => {
                    prompt.status_message = Some(e);
                    prompt.is_installing = false;
                }
            }
        }
        match prompt.show(ctx) {
            craft_update::StartupPromptResponse::Pending => {}
            craft_update::StartupPromptResponse::UpdateAndRestart => {
                prompt.is_installing = true;
                if let Some(install) = craft_update::self_update::current(&craft_update::suite::PHOTOCRAFT) {
                    let mut release = prompt.release.clone();
                    let (tx, rx) = std::sync::mpsc::channel();
                    app.updates_rt.update_rx = Some(rx);
                    let ctx = ctx.clone();
                    std::thread::spawn(move || {
                        let mut run = || -> Result<(), String> {
                            let http = craft_update::Http::new("PhotoCraft/Update").map_err(|e| format!("could not initialize HTTP: {e}"))?;
                            if release.assets.is_empty()
                                && let Some(full) =
                                    craft_update::check_for_update(&craft_update::suite::PHOTOCRAFT, &http, photocraft_engine::build_info::VERSION)
                                        .map_err(|e| format!("could not fetch release assets: {e}"))?
                            {
                                release = full;
                            }
                            let paths = craft_update::Paths::for_app(&craft_update::suite::PHOTOCRAFT).map_err(|e| format!("could not find paths: {e}"))?;
                            let platform = craft_update::Platform::current().ok_or("unsupported platform for self update")?;
                            craft_update::self_update::update(
                                &craft_update::suite::PHOTOCRAFT,
                                &install,
                                &release,
                                platform,
                                &http,
                                &paths,
                                &craft_update::Stop::default(),
                                &mut |_| {},
                                &|| Ok(()),
                                Some(photocraft_engine::build_info::VERSION),
                            )
                            .map_err(|e| e.to_string())
                        };
                        let result = run();
                        if let Err(e) = &result {
                            log::warn!("PhotoCraft self-update failed: {e}");
                        }
                        let _ = tx.send(result);
                        ctx.request_repaint();
                    });
                } else {
                    prompt.status_message = Some("Cannot auto-update this installation type. Please download manually.".to_string());
                    prompt.is_installing = false;
                }
            }
            craft_update::StartupPromptResponse::Download => {
                download = Some(prompt.release.html_url.clone());
                app.updates_rt.prompt = None;
                app.ui.updates_open = false;
            }
            craft_update::StartupPromptResponse::Later => {
                app.updates_rt.prompt = None;
                app.ui.updates_open = false;
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    if app.updates_rt.prompt.is_some() {
        return;
    }

    if !app.ui.updates_open {
        if let Some(url) = download {
            crate::links::open(app, ctx, &url);
        }
        return;
    }
    let mut open = true;
    let status = app.session.updates.status.clone();
    let mut retry = false;
    egui::Window::new(tl!("PhotoCraft Updates"))
        .id(egui::Id::new("photocraft_updates"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(380.0)
        .show(ctx, |ui| {
            let t = crate::theme::Tokens::get(ctx);
            ui.label(crate::i18n::fmt(crate::i18n::t("Installed version: {version}"), &[("version", photocraft_engine::build_info::VERSION)]));
            ui.add_space(8.0);
            match &status {
                Status::Checking => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tl!("Checking for updates…"));
                    });
                }
                Status::Available { release } => {
                    ui.label(crate::i18n::fmt(crate::i18n::t("PhotoCraft {version} is available."), &[("version", &release.version)]));
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        if crate::widgets::primary_button(ui, tl!("Update and Restart"), 180.0).clicked() {
                            let update_rel = craft_update::Release {
                                tag: format!("v{}", release.version),
                                version: craft_update::parse_version(&release.version),
                                name: format!("PhotoCraft {}", release.version),
                                html_url: release.url.clone(),
                                ..Default::default()
                            };
                            let mut prompt =
                                craft_update::StartupPrompt::new(&craft_update::suite::PHOTOCRAFT, update_rel, photocraft_engine::build_info::VERSION);
                            prompt.open = true;
                            app.updates_rt.prompt = Some(prompt);
                        }
                        if ui.button(tl!("Release Notes")).clicked() {
                            download = Some(release.url.clone());
                        }
                    }
                    #[cfg(target_arch = "wasm32")]
                    if crate::widgets::primary_button(ui, tl!("Open Download Page"), 160.0).clicked() {
                        download = Some(release.url.clone());
                    }
                }
                Status::UpToDate => {
                    ui.label(tl!("PhotoCraft is up to date."));
                }
                Status::Failed { message } => {
                    ui.colored_label(t.warning, tl!("Could not check for updates."));
                    ui.label(message);
                }
                Status::Idle => {
                    ui.label(tl!("Updates have not been checked yet."));
                }
            }
            if !matches!(status, Status::Checking) && ui.button(tl!("Check Again")).clicked() {
                retry = true;
            }
        });
    app.ui.updates_open = open;
    if let Some(url) = download {
        crate::links::open(app, ctx, &url);
    }
    if retry {
        let _ = check(app, ctx, serde_json::json!({}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Services, menus};
    use photocraft_engine::{Session, updates_cmds::Release};
    use std::sync::{Arc, Mutex, mpsc};

    type Sender = Arc<Mutex<Option<mpsc::Sender<photocraft_engine::updates_cmds::CheckResult>>>>;

    fn app() -> (PhotocraftApp, Arc<Mutex<u32>>, Sender) {
        let calls = Arc::new(Mutex::new(0));
        let count = calls.clone();
        let sender = Arc::new(Mutex::new(None));
        let tx = sender.clone();
        let app = PhotocraftApp::new(
            Session::new(),
            Services {
                check_updates: Some(Box::new(move || {
                    *count.lock().unwrap() += 1;
                    let (sender, receiver) = mpsc::channel();
                    *tx.lock().unwrap() = Some(sender);
                    Ok(receiver)
                })),
                ..Default::default()
            },
        );
        (app, calls, sender)
    }

    fn at(app: &mut PhotocraftApp, ctx: &egui::Context, time: f64) {
        let mut output = ctx.run_ui(egui::RawInput { time: Some(time), ..Default::default() }, |ui| tick(app, ui.ctx()));
        output.textures_delta.clear();
    }

    #[test]
    fn automatic_check_runs_once_per_launch_even_after_days_or_preference_changes() {
        let (mut app, calls, sender) = app();
        let ctx = egui::Context::default();
        at(&mut app, &ctx, 0.0);
        at(&mut app, &ctx, 1.0);
        at(&mut app, &ctx, 2.0);
        assert_eq!(*calls.lock().unwrap(), 1);
        sender.lock().unwrap().as_ref().unwrap().send(Ok(None)).unwrap();
        at(&mut app, &ctx, 3.0);
        assert!(!app.ui.updates_open, "automatic up-to-date checks stay quiet");
        at(&mut app, &ctx, 86400.0);
        app.session.edit_prefs(|p| p.general.automatically_check_updates = false);
        at(&mut app, &ctx, 86401.0);
        app.session.edit_prefs(|p| p.general.automatically_check_updates = true);
        at(&mut app, &ctx, 172800.0);
        assert_eq!(*calls.lock().unwrap(), 1, "no periodic checks or preference-triggered rechecks");
        let (mut restarted, restarted_calls, _) = self::app();
        at(&mut restarted, &ctx, 0.0);
        assert_eq!(*restarted_calls.lock().unwrap(), 1, "the next launch checks again");
    }

    #[test]
    fn startup_opt_out_and_failed_checks_do_not_retry_automatically() {
        let (mut disabled, calls, _) = app();
        let ctx = egui::Context::default();
        disabled.session.edit_prefs(|p| p.general.automatically_check_updates = false);
        at(&mut disabled, &ctx, 0.0);
        disabled.session.edit_prefs(|p| p.general.automatically_check_updates = true);
        at(&mut disabled, &ctx, 86400.0);
        assert_eq!(*calls.lock().unwrap(), 0, "opt-out is evaluated at startup");

        let (mut failed, calls, sender) = app();
        at(&mut failed, &ctx, 0.0);
        sender.lock().unwrap().as_ref().unwrap().send(Err("offline".into())).unwrap();
        at(&mut failed, &ctx, 1.0);
        at(&mut failed, &ctx, 172800.0);
        assert_eq!(*calls.lock().unwrap(), 1);
        assert!(!failed.ui.updates_open, "an automatic failure stays quiet");
    }

    #[test]
    fn manual_check_works_when_automatic_checks_are_off_and_opens_status() {
        let (mut app, calls, sender) = app();
        let ctx = egui::Context::default();
        app.session.edit_prefs(|p| p.general.automatically_check_updates = false);
        assert!(menus::is_enabled(&app, "help.checkForUpdates"));
        menus::invoke(&mut app, &ctx, "help.checkForUpdates", serde_json::json!({})).unwrap();
        assert!(app.ui.updates_open);
        assert_eq!(*calls.lock().unwrap(), 1);
        sender.lock().unwrap().as_ref().unwrap().send(Err("offline".into())).unwrap();
        at(&mut app, &ctx, 1.0);
        assert!(matches!(app.session.updates.status, Status::Failed { .. }));
        let mut web = PhotocraftApp::new(Session::new(), Services::default());
        assert!(!menus::is_enabled(&web, "help.checkForUpdates"));
        at(&mut web, &ctx, 2.0);
        assert_eq!(web.session.updates.status, Status::Idle);
    }

    #[test]
    fn available_release_notifies_once_even_when_automation_polls_first() {
        let (mut app, _, sender) = app();
        let ctx = egui::Context::default();
        at(&mut app, &ctx, 0.0);
        sender
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .send(Ok(Some(Release { version: "99.0.0".into(), url: "https://github.com/storytold/photocraft/releases/tag/v99.0.0".into() })))
            .unwrap();
        app.run("updates.status", serde_json::json!({})).unwrap();
        at(&mut app, &ctx, 1.0);
        assert!(app.ui.updates_open);
        app.ui.updates_open = false;
        at(&mut app, &ctx, 2.0);
        assert!(!app.ui.updates_open);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn startup_update_prompt_appears_and_can_be_dismissed() {
        let (mut app, _, sender) = app();
        let ctx = egui::Context::default();
        at(&mut app, &ctx, 0.0);
        sender
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .send(Ok(Some(Release { version: "99.0.0".into(), url: "https://github.com/storytold/photocraft/releases/tag/v99.0.0".into() })))
            .unwrap();
        at(&mut app, &ctx, 1.0);
        assert!(app.updates_rt.prompt.is_some(), "startup prompt initialized on available release");
        let prompt = app.updates_rt.prompt.as_ref().unwrap();
        assert_eq!(prompt.release.tag, "v99.0.0");
        assert!(prompt.open);

        // Rendering show() renders the prompt modal
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(&mut app, ui.ctx());
        });
        output.textures_delta.clear();
        assert!(app.updates_rt.prompt.is_some());
    }

    #[test]
    fn a_manual_check_that_fails_to_start_opens_the_status_window() {
        let mut app =
            PhotocraftApp::new(Session::new(), Services { check_updates: Some(Box::new(|| Err("Could not start worker".into()))), ..Default::default() });
        let ctx = egui::Context::default();
        let error = menus::invoke(&mut app, &ctx, "help.checkForUpdates", serde_json::json!({})).unwrap_err();
        assert!(error.contains("Could not start worker"), "{error}");
        assert!(matches!(app.session.updates.status, Status::Failed { .. }));
        assert!(app.ui.updates_open, "the failure and Check Again are shown");
        // Bad params are a caller error, not a failed check: nothing opens, also while the earlier
        // failure is still the status.
        app.ui.updates_open = false;
        assert!(menus::invoke(&mut app, &ctx, "help.checkForUpdates", serde_json::json!({"x": 1})).is_err());
        assert!(matches!(app.session.updates.status, Status::Failed { .. }), "the earlier failure stays");
        assert!(!app.ui.updates_open, "bad params after a failed check");
        app.session.updates.status = Status::Idle;
        assert!(menus::invoke(&mut app, &ctx, "help.checkForUpdates", serde_json::json!({"x": 1})).is_err());
        assert!(!app.ui.updates_open);
        // The same start failure again opens it again.
        app.session.updates.status = Status::Failed { message: "Could not start worker".into() };
        assert!(menus::invoke(&mut app, &ctx, "help.checkForUpdates", serde_json::json!({})).is_err());
        assert!(app.ui.updates_open, "a repeated start failure");
    }

    #[test]
    fn update_window_is_drivable_and_bad_state_changes_are_atomic() {
        let (mut app, _, _) = app();
        let ctx = egui::Context::default();
        let call = |app: &mut PhotocraftApp, method: &str, params: Value| {
            let (req, _) = crate::control::ControlRequest::new(method, params);
            match crate::control::handle(app, &ctx, &req) {
                crate::control::Outcome::Done(value) => value,
                _ => panic!("expected an immediate reply"),
            }
        };
        assert_eq!(call(&mut app, "ui.set", serde_json::json!({"updatesOpen": true}))["ok"], true);
        let inspected = call(&mut app, "ui.inspect", serde_json::json!({}));
        assert_eq!(inspected["result"]["updatesOpen"], true);
        assert_eq!(inspected["result"]["updates"]["supported"], true);
        assert_eq!(call(&mut app, "ui.set", serde_json::json!({"updatesOpen": false, "maskTarget": "bad"}))["ok"], false);
        assert!(app.ui.updates_open);
        assert_eq!(call(&mut app, "ui.set", serde_json::json!({"updatesOpen": false}))["ok"], true);
        assert!(!app.ui.updates_open);
    }
}
