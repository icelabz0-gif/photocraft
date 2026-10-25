//! Monitor choices use the existing command/job and preference transactions. Dialog drafts
//! are serializable; workers, cancellation and automation provenance stay in runtime state.
use crate::{PhotocraftApp, state::DialogKind};
use photocraft_engine::jobs::{JobEvent, JobId, JobOutcome};
use photocraft_engine::monitor_cmds::{COMMAND, DEFAULT_DISPLAY, Snapshot};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::sync::{Arc, mpsc::Receiver};

pub type CatalogReader = Arc<dyn Fn() -> Option<Receiver<photocraft_engine::installed_profiles::MonitorCatalog>> + Send + Sync>;
#[derive(Default)]
pub struct Runtime {
    catalog: Option<photocraft_engine::installed_profiles::MonitorCatalog>,
    reading: Option<Receiver<photocraft_engine::installed_profiles::MonitorCatalog>>,
    catalog_error: Option<String>,
    apply: Option<Pending>,
    refresh: Option<JobId>,
    refresh_again: bool,
    initialized: bool,
    restricted: BTreeSet<u64>,
}
struct Pending {
    job: JobId,
    dialog: u64,
    close: bool,
    before: Snapshot,
}

pub fn owns(f: &Map<String, Value>) -> bool {
    f.get("__command").and_then(Value::as_str) == Some(COMMAND)
}
fn choices(app: &PhotocraftApp) -> Value {
    let mut choices = serde_json::to_value(&app.session.color.settings.monitor_overrides).unwrap_or(json!({}));
    if let Some(m) = choices.as_object_mut() {
        m.insert(DEFAULT_DISPLAY.into(), json!(app.session.color.settings.monitor_profile));
    }
    choices
}
pub fn open(app: &mut PhotocraftApp) -> u64 {
    if let Some(id) = app.ui.dialogs.iter().find(|d| owns(&d.fields)).map(|d| d.id) {
        restrict_automated(app, id);
        return id;
    }
    let mut f = Map::new();
    f.insert("__command".into(), json!(COMMAND));
    f.insert("__label".into(), json!("Monitor Profiles…"));
    let target = app
        .session
        .color
        .displays
        .iter()
        .find(|d| Some(d.id) == app.session.color.main_display)
        .filter(|d| app.session.color.display_identity(d) == "ok")
        .and_then(|d| d.key.as_deref())
        .unwrap_or(DEFAULT_DISPLAY);
    f.insert("target".into(), json!(target));
    f.insert("choices".into(), choices(app));
    f.insert("baseline".into(), choices(app));
    let id = app.ui.open_dialog(DialogKind::Command, f);
    if app.automation_input || app.session.authorize.is_some() {
        app.monitor_profiles.restricted.insert(id);
    } else {
        refresh(app);
        let _ = read_catalog(app, id);
        crate::monitor_status::read_now(app);
    }
    id
}
/// Restriction belongs to the dialog lifetime, not just the frame that edited it.
fn restrict_automated(app: &mut PhotocraftApp, id: u64) {
    if app.automation_input || app.session.authorize.is_some() {
        app.monitor_profiles.restricted.insert(id);
    }
}

fn authorize_draft(app: &PhotocraftApp, params: &Value) -> Result<(), String> {
    if let Some(gate) = app.session.authorize.or(app.services.automation_authorize) {
        gate(COMMAND, params).map_err(|e| e.to_string())?;
    }
    if let Some(gate) = &app.services.automation_command {
        gate(COMMAND, params)?;
    }
    // Also fail closed in a shell without an automation service. These are command params
    // built from bounded draft maps, never arbitrary nested input or working-space aliases.
    if params["set"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|change| !change["profile"].as_str().is_some_and(|p| p == "auto" || photocraft_engine::monitor_cmds::builtin(p).is_some()))
    {
        return Err("Automated dialogs cannot choose ICC files or profile aliases".into());
    }
    Ok(())
}

/// The only control-channel edits of this dialog. Routing and the saved baseline are
/// immutable, and an accepted automation edit remains restricted on later human clicks.
pub fn set_from_control(app: &mut PhotocraftApp, id: u64, field: &str, value: Value) -> Result<Value, String> {
    let d = app.ui.dialogs.iter().find(|d| d.id == id && owns(&d.fields)).ok_or("monitor dialog is closed")?;
    let valid_key = |key: &str| !key.is_empty() && key.len() <= 128 && !key.contains('\0');
    match field {
        "target" if value.as_str().is_some_and(valid_key) => {}
        "choices"
            if value.as_object().is_some_and(|choices| {
                choices.len() <= 65
                    && choices.contains_key(DEFAULT_DISPLAY)
                    && choices
                        .iter()
                        .all(|(key, profile)| valid_key(key) && profile.as_str().is_some_and(|p| !p.is_empty() && p.len() <= 4096 && !p.contains('\0')))
            }) => {}
        _ => return Err("Monitor dialog automation can edit only bounded target and choices fields".into()),
    }
    let mut next = d.fields.clone();
    next.insert(field.into(), value);
    authorize_draft(app, &params(&next)?)?;
    app.monitor_profiles.restricted.insert(id);
    if let Some(d) = app.ui.dialog_mut(id) {
        d.fields = next;
    }
    Ok(Value::Null)
}

fn restricted(app: &PhotocraftApp, id: u64) -> bool {
    app.automation_input || app.session.authorize.is_some() || app.monitor_profiles.restricted.contains(&id)
}
fn read_catalog(app: &mut PhotocraftApp, id: u64) -> Result<(), String> {
    if restricted(app, id) {
        return Err("Automated dialogs cannot scan profiles or choose ICC files".into());
    }
    if app.monitor_profiles.reading.is_none() {
        app.monitor_profiles.catalog_error = None;
        app.monitor_profiles.reading = app.services.monitor_catalog.as_ref().and_then(|read| read());
    }
    Ok(())
}
/// Triggered by actual app startup/focus/display reads, never by a synthetic dialog action.
pub fn refresh(app: &mut PhotocraftApp) {
    if app.automation_input || app.session.authorize.is_some() {
        return;
    }
    app.monitor_profiles.refresh_again = true;
}
pub fn poll(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !app.monitor_profiles.initialized {
        app.monitor_profiles.initialized = true;
        // Native services distinguish a real app from headless/automation sessions.
        if app.services.monitor_catalog.is_some() {
            refresh(app);
        }
    }
    if let Some(rx) = &app.monitor_profiles.reading {
        match rx.try_recv() {
            Ok(catalog) => {
                app.monitor_profiles.catalog = Some(catalog);
                app.monitor_profiles.reading = None;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                app.monitor_profiles.reading = None;
                app.monitor_profiles.catalog_error = Some("Profile search stopped without an answer".into());
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
        }
    }
    if app.monitor_profiles.refresh_again
        && app.monitor_profiles.refresh.is_none()
        && app.monitor_profiles.apply.is_none()
        && !app.automation_input
        && app.session.authorize.is_none()
    {
        app.monitor_profiles.refresh_again = false;
        let status = app.ui.status.clone();
        let status_error = app.ui.status_error;
        match app.run(COMMAND, json!({"refresh":true})) {
            Ok(v) => {
                app.monitor_profiles.refresh = v["job"].as_u64().map(JobId);
                // An automatic refresh must not acknowledge an unrelated unread error.
                app.ui.status = status;
                app.ui.status_error = status_error;
            }
            Err(e) => {
                app.ui.status = e;
                app.ui.status_error = true;
            }
        }
    }
}

pub fn params(f: &Map<String, Value>) -> Result<Value, String> {
    let next = f.get("choices").and_then(Value::as_object).ok_or("missing monitor choices")?;
    let base = f.get("baseline").and_then(Value::as_object).ok_or("missing monitor baseline")?;
    if next.len() > 65 || base.len() > 65 {
        return Err("too many monitor choices".into());
    }
    let set: Vec<Value> =
        next.iter().filter(|(key, value)| base.get(*key) != Some(*value)).map(|(key, value)| json!({"display":key,"profile":value})).collect();
    let clear: Vec<&String> = base.keys().filter(|key| !next.contains_key(*key)).collect();
    Ok(json!({"set":set,"clear":clear}))
}
pub fn busy(app: &PhotocraftApp) -> bool {
    app.monitor_profiles.apply.is_some()
}
pub fn changed(f: &Map<String, Value>) -> bool {
    f.get("choices") != f.get("baseline")
}
fn error(app: &mut PhotocraftApp, id: u64, message: &str) {
    if let Some(d) = app.ui.dialog_mut(id) {
        d.fields.insert("__error".into(), json!(message));
    }
    app.ui.status = message.into();
    app.ui.status_error = true;
}
pub fn apply(app: &mut PhotocraftApp, id: u64, close: bool) -> Result<Value, String> {
    if busy(app) {
        return Err("Monitor profiles are still being applied".into());
    }
    let f = app.ui.dialogs.iter().find(|d| d.id == id).ok_or("monitor dialog is closed")?.fields.clone();
    let params = params(&f)?;
    if restricted(app, id)
        && let Err(e) = authorize_draft(app, &params)
    {
        error(app, id, &e);
        return Err(e);
    }
    if let Some(job) = app.monitor_profiles.refresh {
        // Keep ownership until its queued event arrives, so automatic cancellation cannot
        // overwrite an unread status through the generic job handler.
        app.session.cancel_job(job);
        app.monitor_profiles.refresh_again = true;
    }
    let before = Snapshot::capture(&app.session);
    // A dialog opened by automation stays restricted even when its callback runs later.
    let previous_input = app.automation_input;
    app.automation_input = restricted(app, id);
    let result = app.run(COMMAND, params);
    app.automation_input = previous_input;
    match result {
        Ok(v) if v["job"].as_u64().is_some() => {
            if let Some(job) = v["job"].as_u64() {
                app.monitor_profiles.apply = Some(Pending { job: JobId(job), dialog: id, close, before });
            }
            Ok(v)
        }
        Ok(v) => {
            finish(app, id, close, before)?;
            Ok(v)
        }
        Err(e) => {
            error(app, id, &e);
            Err(e)
        }
    }
}
fn finish(app: &mut PhotocraftApp, id: u64, close: bool, before: Snapshot) -> Result<(), String> {
    if let Err(e) = crate::prefs_ui::save_preferences(app) {
        before.restore(&mut app.session);
        error(app, id, &e);
        return Err(e);
    }
    let now = choices(app);
    if let Some(d) = app.ui.dialog_mut(id) {
        d.fields.insert("choices".into(), now.clone());
        d.fields.insert("baseline".into(), now);
        d.fields.remove("__error");
    }
    if close {
        app.ui.close_dialog(id);
        app.monitor_profiles.restricted.remove(&id);
    }
    Ok(())
}
/// Complete a monitor job before its control waiters receive a reply. A successful worker
/// is not a successful Apply until preferences have also been saved. The success value allows
/// a completion beep only for a persisted Apply; internal refreshes stay silent.
pub fn on_event(app: &mut PhotocraftApp, e: &JobEvent) -> Option<Result<bool, String>> {
    if app.monitor_profiles.refresh == Some(e.id) {
        app.monitor_profiles.refresh = None;
        if let JobOutcome::Done(v) = &e.outcome {
            app.monitor_profiles.refresh_again |= v["stale"] == true;
        }
        return Some(Ok(false));
    }
    if app.monitor_profiles.apply.as_ref().is_some_and(|p| p.job == e.id) {
        if let Some(p) = app.monitor_profiles.apply.take() {
            match &e.outcome {
                JobOutcome::Done(_) => {
                    return Some(finish(app, p.dialog, p.close, p.before).map(|()| true));
                }
                JobOutcome::Failed(e) => error(app, p.dialog, e),
                JobOutcome::Cancelled => {}
            }
        }
        return Some(Ok(false));
    }
    None
}
pub fn cancel(app: &mut PhotocraftApp, id: u64) {
    if app.monitor_profiles.apply.as_ref().is_some_and(|p| p.dialog == id)
        && let Some(p) = app.monitor_profiles.apply.take()
    {
        app.session.cancel_job(p.job);
    }
    app.monitor_profiles.restricted.remove(&id);
}

pub fn body(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>, id: u64) {
    restrict_automated(app, id);
    if cfg!(target_arch = "wasm32") {
        ui.label(crate::monitor_status::note(app));
        return;
    }
    let t = crate::theme::Tokens::get(ui.ctx());
    let mut target = f.get("target").and_then(Value::as_str).unwrap_or(DEFAULT_DISPLAY).to_string();
    let mut targets = vec![(DEFAULT_DISPLAY.to_string(), tl!("All displays (default)").to_string())];
    for d in &app.session.color.displays {
        if app.session.color.display_identity(d) == "ok"
            && let Some(key) = &d.key
        {
            targets.push((key.clone(), d.name.clone()));
        }
    }
    // Disconnected saved choices stay visible and can be removed, never silently reassigned.
    if let Some(c) = f.get("choices").and_then(Value::as_object) {
        for key in c.keys() {
            if !targets.iter().any(|(k, _)| k == key) {
                targets.push((key.clone(), format!("{} ({})", tl!("Disconnected display"), key.get(..18).unwrap_or(key))));
            }
        }
    }
    if !targets.iter().any(|(k, _)| k == &target) {
        targets.push((target.clone(), tl!("Disconnected display").into()));
    }
    ui.spacing_mut().item_spacing.y = 7.0;
    ui.add_enabled_ui(!busy(app), |ui| {
        ui.horizontal(|ui| {
            ui.add_sized([55.0, 22.0], egui::Label::new(tl!("Display:")));
            let opts: Vec<(String, &str)> = targets.iter().map(|(k, n)| (k.clone(), n.as_str())).collect();
            crate::widgets::dropdown(ui, "monitor-target", &mut target, &opts, 300.0);
        });
        f.insert("target".into(), json!(target));
        let mut selected = f.get("choices").and_then(|c| c.get(&target)).and_then(Value::as_str).unwrap_or("").to_string();
        let mut options: Vec<(String, String)> = vec![];
        if target != DEFAULT_DISPLAY {
            options.push(("".into(), tl!("Use default").into()));
        }
        options.push(("auto".into(), tl!("Automatic").into()));
        for b in photocraft_cms::Builtin::ALL {
            if b.profile().color_space == photocraft_cms::ColorSpace::Rgb {
                options.push((b.id().into(), b.description().into()));
            }
        }
        if let Some(catalog) = &app.monitor_profiles.catalog {
            for p in &catalog.profiles {
                options.push((p.path.clone(), p.description.clone()));
            }
        }
        if !options.iter().any(|(k, _)| k == &selected) {
            options.push((selected.clone(), selected.clone()));
        }
        ui.horizontal(|ui| {
            ui.add_sized([55.0, 22.0], egui::Label::new(tl!("Profile:")));
            let opts: Vec<(String, &str)> = options.iter().map(|(k, n)| (k.clone(), n.as_str())).collect();
            if crate::widgets::dropdown(ui, "monitor-profile", &mut selected, &opts, 300.0)
                && let Some(c) = f.get_mut("choices").and_then(Value::as_object_mut)
            {
                if selected.is_empty() {
                    c.remove(&target);
                } else {
                    c.insert(target.clone(), json!(selected));
                }
            }
        });
        ui.horizontal(|ui| {
            if crate::widgets::secondary_button(ui, tl!("Choose ICC File…"), 142.0).clicked() {
                if restricted(app, id) {
                    f.insert("__error".into(), json!("Automated dialogs cannot choose ICC files"));
                } else {
                    let pinned = target.clone();
                    let result = app.ask_file(
                        crate::file_dialog::FileDialogRequest::Open { multiple: false, initial_dir: None, extensions: Some(vec!["icc".into(), "icm".into()]) },
                        move |app, answer| {
                            restrict_automated(app, id);
                            if restricted(app, id) {
                                return Err("Automated dialogs cannot choose ICC files".into());
                            }
                            let crate::file_dialog::FileDialogAnswer::Paths(paths) = answer else {
                                return Err("Choose an ICC file on disk".into());
                            };
                            let path = paths.first().filter(|p| p.len() <= 4096).ok_or("invalid ICC file path")?;
                            if let Some(d) = app.ui.dialog_mut(id)
                                && let Some(c) = d.fields.get_mut("choices").and_then(Value::as_object_mut)
                            {
                                c.insert(pinned, json!(path));
                            }
                            Ok(Value::Null)
                        },
                    );
                    if let Err(e) = result {
                        f.insert("__error".into(), json!(e));
                    }
                }
            }
            if crate::widgets::secondary_button(ui, tl!("Refresh"), 84.0).clicked() {
                if let Err(e) = read_catalog(app, id) {
                    f.insert("__error".into(), json!(e));
                } else {
                    refresh(app);
                    crate::monitor_status::read_now(app);
                }
            }
        });
    });
    if let Some(spec) = f
        .get("choices")
        .and_then(|c| c.get(&target))
        .and_then(Value::as_str)
        .filter(|s| !photocraft_engine::monitor_cmds::automatic(s) && photocraft_engine::monitor_cmds::builtin(s).is_none())
    {
        ui.add(egui::Label::new(egui::RichText::new(spec).small().color(t.text_dim)).truncate()).on_hover_text(spec);
    }
    let display = app.session.color.displays.iter().find(|d| d.key.as_ref() == Some(&target));
    let status = app.session.color.monitor_status_for(display.map(|d| d.id));
    crate::widgets::hairline(ui);
    ui.label(egui::RichText::new(tl!("Currently applied:")).color(t.text_dim));
    ui.add(egui::Label::new(status.summary()).wrap()).on_hover_text(format!("{}\n{}", status.requested, status.fingerprint));
    if status.loading || app.monitor_profiles.refresh.is_some() || app.monitor_profiles.reading.is_some() {
        ui.label(tl!("Reading profiles…"));
    }
    if display.is_none() && target != DEFAULT_DISPLAY {
        ui.label(tl!("Reconnect this display to change its profile. Use default removes the saved choice."));
    }
    if app.session.color.displays.iter().any(|d| app.session.color.display_identity(d) != "ok") {
        ui.add(egui::Label::new(tl!("Displays without a unique identity use the default.")).wrap());
    }
    ui.add(
        egui::Label::new(
            egui::RichText::new(tl!("Search covers standard profile folders. Other profiles can be chosen as ICC files.")).small().color(t.text_dim),
        )
        .wrap(),
    );
    if app.monitor_profiles.catalog.as_ref().is_some_and(|c| c.truncated) {
        ui.label(tl!("Profile search reached its limit."));
    }
    if let Some(e) = app.monitor_profiles.catalog_error.as_deref().or_else(|| f.get("__error").and_then(Value::as_str)) {
        ui.add(egui::Label::new(e).wrap());
    }
    if busy(app) {
        ui.label(tl!("Applying monitor profiles…"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> PhotocraftApp {
        PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default())
    }
    fn choose(app: &mut PhotocraftApp, id: u64, profile: &str) {
        app.ui.dialog_mut(id).unwrap().fields.get_mut("choices").unwrap()[DEFAULT_DISPLAY] = json!(profile);
    }
    #[test]
    fn apply_keeps_dialog_cancel_discards_only_later_draft_and_ok_closes() {
        let mut app = app();
        let id = open(&mut app);
        choose(&mut app, id, "display-p3");
        apply(&mut app, id, false).unwrap();
        assert_eq!(app.session.color.settings.monitor_profile, "display-p3");
        assert!(app.ui.dialog_mut(id).is_some());
        choose(&mut app, id, "srgb");
        crate::dialogs::cancel(&mut app, id).unwrap();
        assert_eq!(app.session.color.settings.monitor_profile, "display-p3");
        let id = open(&mut app);
        choose(&mut app, id, "srgb");
        crate::dialogs::confirm(&mut app, id).unwrap();
        assert!(app.ui.dialog_mut(id).is_none());
        assert_eq!(app.session.color.settings.monitor_profile, "srgb");
    }
    #[test]
    fn save_failure_rolls_back_monitor_only_and_keeps_draft_and_error() {
        let mut app = app();
        app.services.save_prefs = Some(Box::new(|_| Err("disk full".into())));
        let id = open(&mut app);
        choose(&mut app, id, "display-p3");
        assert!(apply(&mut app, id, true).is_err());
        assert_eq!(app.session.color.settings.monitor_profile, "auto");
        let fields = &app.ui.dialog_mut(id).unwrap().fields;
        assert_eq!(fields["choices"][DEFAULT_DISPLAY], "display-p3");
        assert_eq!(fields["__error"], "disk full");
    }
    #[test]
    fn waiting_control_reply_includes_monitor_preference_save_result() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static BEEPS: AtomicUsize = AtomicUsize::new(0);
        fn count_beep() {
            BEEPS.fetch_add(1, Ordering::SeqCst);
        }
        for method in ["ui.dialog.apply", "ui.dialog.confirm"] {
            for fail_save in [true, false] {
                BEEPS.store(0, Ordering::SeqCst);
                let mut app = app();
                app.background_jobs = true;
                app.services.beep = Some(count_beep);
                app.session.edit_prefs(|p| p.general.beep_when_done = true);
                let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let count = Arc::clone(&writes);
                app.services.save_prefs = Some(Box::new(move |_| {
                    assert_eq!(BEEPS.load(Ordering::SeqCst), 0, "do not signal success before preferences are saved");
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if fail_save { Err("disk full".into()) } else { Ok(()) }
                }));
                let id = open(&mut app);
                choose(&mut app, id, "display-p3");
                let ctx = egui::Context::default();
                let (request, rx) = crate::control::ControlRequest::new(method, json!({"dialog":id}));
                let crate::control::Outcome::AfterJob(job) = crate::control::handle(&mut app, &ctx, &request) else {
                    panic!("expected asynchronous monitor apply");
                };
                app.jobs.waiters.push((job, request.reply));
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                let reply = loop {
                    crate::jobs_ui::tick(&mut app, &ctx);
                    if let Ok(reply) = rx.try_recv() {
                        break reply;
                    }
                    assert!(std::time::Instant::now() < deadline, "monitor apply did not reply");
                    std::thread::sleep(std::time::Duration::from_millis(1));
                };
                assert_eq!(writes.load(std::sync::atomic::Ordering::SeqCst), 1);
                assert_eq!(BEEPS.load(Ordering::SeqCst), usize::from(!fail_save), "only a persisted Apply should beep");
                assert_eq!(reply["ok"], !fail_save, "{method}: {reply}");
                assert!(!busy(&app));
                if fail_save {
                    assert_eq!(reply["error"], "disk full");
                    assert_eq!(app.session.color.settings.monitor_profile, "auto");
                    let fields = &app.ui.dialog_mut(id).unwrap().fields;
                    assert_eq!(fields["choices"][DEFAULT_DISPLAY], "display-p3");
                    assert_eq!(fields["baseline"][DEFAULT_DISPLAY], "auto");
                    assert_eq!(fields["__error"], "disk full");
                } else {
                    assert_eq!(app.session.color.settings.monitor_profile, "display-p3");
                    assert_eq!(app.ui.dialog_mut(id).is_none(), method == "ui.dialog.confirm");
                }
            }
        }
    }

    #[test]
    fn background_monitor_refresh_preserves_unread_status() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static BEEPS: AtomicUsize = AtomicUsize::new(0);
        fn count_beep() {
            BEEPS.fetch_add(1, Ordering::SeqCst);
        }
        for background in [false, true] {
            BEEPS.store(0, Ordering::SeqCst);
            let mut app = app();
            app.background_jobs = background;
            app.services.beep = Some(count_beep);
            app.session.edit_prefs(|p| p.general.beep_when_done = true);
            app.ui.status = "Previous export failed".into();
            app.ui.status_error = true;
            let ctx = egui::Context::default();
            refresh(&mut app);
            poll(&mut app, &ctx);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while app.session.has_jobs() {
                crate::jobs_ui::tick(&mut app, &ctx);
                assert!(std::time::Instant::now() < deadline, "monitor refresh did not finish");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert_eq!(BEEPS.load(Ordering::SeqCst), 0, "automatic monitor refresh must stay silent");
            assert_eq!(app.ui.status, "Previous export failed");
            assert!(app.ui.status_error);
            assert!(app.monitor_profiles.refresh.is_none());
        }
    }

    #[test]
    fn applying_during_refresh_preserves_status_when_cancellation_arrives() {
        for close in [false, true] {
            for invalid in [false, true] {
                let mut app = app();
                app.background_jobs = true;
                let ctx = egui::Context::default();
                let id = open(&mut app);
                poll(&mut app, &ctx);
                let refresh = app.monitor_profiles.refresh.unwrap();
                assert!(app.session.job(refresh).is_some());
                choose(&mut app, id, if invalid { "" } else { "display-p3" });
                assert_eq!(apply(&mut app, id, close).is_err(), invalid);
                let status = app.ui.status.clone();
                let status_error = app.ui.status_error;
                assert_eq!(status_error, invalid);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    // Apply has cancelled the refresh, but its completion event is still queued.
                    crate::jobs_ui::tick(&mut app, &ctx);
                    if !app.session.has_jobs() {
                        break;
                    }
                    assert!(std::time::Instant::now() < deadline, "monitor apply did not finish");
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                assert_eq!(app.ui.status, status, "close={close}, invalid={invalid}");
                assert_eq!(app.ui.status_error, status_error);
                assert!(app.monitor_profiles.refresh.is_none());
                assert!(!busy(&app));
                assert_eq!(app.session.color.settings.monitor_profile, if invalid { "auto" } else { "display-p3" });
                assert_eq!(app.ui.dialog_mut(id).is_none(), close && !invalid);
                // Consuming the old event also allows the requested replacement refresh to run.
                poll(&mut app, &ctx);
                assert!(app.monitor_profiles.refresh.is_some());
                assert_ne!(app.monitor_profiles.refresh, Some(refresh));
            }
        }
    }

    #[test]
    fn cancelling_pending_apply_never_commits_and_stale_dialog_remains_pinned() {
        let mut app = app();
        app.background_jobs = true;
        let id = open(&mut app);
        choose(&mut app, id, "display-p3");
        let job = apply(&mut app, id, true).unwrap()["job"].as_u64().unwrap();
        crate::dialogs::cancel(&mut app, id).unwrap();
        assert!(app.session.wait_job(JobId(job)).is_err());
        assert_eq!(app.session.color.settings.monitor_profile, "auto");
    }
    #[test]
    fn automation_open_does_not_scan_and_cannot_launder_later_file_change() {
        let mut app = app();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        app.services.monitor_catalog = Some(Arc::new(move || {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            None
        }));
        fn gate(_: &str, p: &Value) -> photocraft_engine::Result<()> {
            if p["set"].as_array().into_iter().flatten().any(|c| c["profile"].as_str().is_some_and(|p| p.ends_with(".icc"))) {
                Err(photocraft_engine::EngineError::Other("no ambient ICC files".into()))
            } else {
                Ok(())
            }
        }
        app.services.automation_authorize = Some(gate);
        app.session.authorize = Some(gate);
        let id = open(&mut app);
        app.session.authorize = None;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(read_catalog(&mut app, id).is_err());
        choose(&mut app, id, "/not-read.icc");
        assert!(apply(&mut app, id, false).unwrap_err().contains("no ambient"));
        assert_eq!(app.session.color.settings.monitor_profile, "auto");
    }
    fn control(app: &mut PhotocraftApp, method: &str, params: Value) -> Value {
        let (request, _) = crate::control::ControlRequest::new(method, params);
        match crate::control::handle(app, &egui::Context::default(), &request) {
            crate::control::Outcome::Done(value) => value,
            _ => panic!("expected immediate monitor control response"),
        }
    }

    #[test]
    fn control_cannot_stage_monitor_files_or_rewrite_routing_and_baseline() {
        let mut app = app();
        let id = open(&mut app);
        let original = app.ui.dialog_mut(id).unwrap().fields.clone();
        for (field, value) in [
            ("choices", json!({"default": "/outside/monitor.icc"})),
            ("choices", json!({"default": "working"})),
            ("choices", json!({"default": {"profile": "srgb"}})),
            ("choices", json!({})),
            ("baseline", json!({"default": "/outside/monitor.icc"})),
            ("__command", json!("file.open")),
            ("__command", Value::Null),
            ("__label", json!("Innocent dialog")),
        ] {
            let result = control(&mut app, "ui.dialog.set", json!({"dialog":id,"field":field,"value":value}));
            assert_eq!(result["ok"], false, "{result}");
            assert_eq!(app.ui.dialog_mut(id).unwrap().fields, original);
        }
        let result = control(&mut app, "ui.dialog.set", json!({"dialog":id,"field":"choices","value":{"default":"display-p3"}}));
        assert_eq!(result["ok"], true, "{result}");
        assert!(app.monitor_profiles.restricted.contains(&id));
        // A native Apply still accepts the permitted built-in edit after control::handle restored
        // the request's temporary authorization. A later injected file remains refused.
        apply(&mut app, id, false).unwrap();
        assert_eq!(app.session.color.settings.monitor_profile, "display-p3");
        choose(&mut app, id, "/outside/monitor.icc");
        assert!(crate::dialogs::confirm(&mut app, id).unwrap_err().contains("Automated dialogs"));
        assert_eq!(app.session.color.settings.monitor_profile, "display-p3");
        assert!(app.ui.dialog_mut(id).is_some());
    }

    #[test]
    fn control_cannot_forge_a_monitor_dialog_or_adopt_one_without_restriction() {
        let mut app = app();
        app.services.automation_authorize = Some(|_, _| Ok(()));
        for kind in ["about", "newDocument"] {
            let result = control(
                &mut app,
                "ui.dialog.open",
                json!({"kind":kind,"fields":{
                    "__command":COMMAND,"baseline":{"default":"auto"},"choices":{"default":"/outside/monitor.icc"}
                }}),
            );
            assert_eq!(result["ok"], false, "{result}");
            assert!(app.ui.dialogs.is_empty());
        }
        let other = control(&mut app, "ui.dialog.open", json!({"kind":"command","command":"test.unrelated"}))["result"]["dialog"].as_u64().unwrap();
        let result = control(&mut app, "ui.dialog.set", json!({"dialog":other,"field":"__command","value":COMMAND}));
        assert_eq!(result["ok"], false, "{result}");
        crate::dialogs::cancel(&mut app, other).unwrap();
        let id = open(&mut app);
        assert!(!restricted(&app, id));
        let result = control(&mut app, "ui.dialog.open", json!({"kind":"command","command":COMMAND}));
        assert_eq!(result["result"]["dialog"], id, "{result}");
        assert!(app.session.authorize.is_none());
        assert!(restricted(&app, id));
        assert!(read_catalog(&mut app, id).is_err());
        choose(&mut app, id, "/outside/monitor.icc");
        assert!(apply(&mut app, id, false).is_err());
        assert_eq!(app.session.color.settings.monitor_profile, "auto");
    }

    fn draw_test_dialog(ui: &mut egui::Ui, state: &mut (PhotocraftApp, u64)) {
        // Harness construction draws before setup_context can install the named UI fonts.
        if !ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
            return;
        }
        let (app, id) = state;
        let mut fields = app.ui.dialog_mut(*id).unwrap().fields.clone();
        body(app, ui, &mut fields, *id);
        app.ui.dialog_mut(*id).unwrap().fields = fields;
    }

    #[test]
    fn synthetic_dropdown_edit_remains_restricted_for_a_later_native_apply() {
        use egui_kittest::{Harness, kittest::Queryable};
        let mut app = app();
        let id = open(&mut app);
        app.monitor_profiles.catalog = Some(photocraft_engine::installed_profiles::MonitorCatalog {
            profiles: vec![photocraft_engine::installed_profiles::MonitorCatalogProfile {
                path: "/outside/monitor.icc".into(),
                description: "ZZZ calibration".into(),
                fingerprint: "test".into(),
            }],
            ..Default::default()
        });
        let mut h = Harness::builder().with_size(egui::vec2(640.0, 500.0)).build_ui_state(draw_test_dialog, (app, id));
        PhotocraftApp::setup_context(&h.ctx, Default::default());
        h.run_steps(3);
        // Open the Profile dropdown, then feed actual control-channel text through the
        // same synthetic-input drain the desktop uses. Type-ahead chooses the ICC entry.
        h.get_all_by_role(egui::accesskit::Role::ComboBox).nth(1).unwrap().click();
        h.run();
        let ctx = h.ctx.clone();
        let (request, _) = crate::control::ControlRequest::new("ui.type", json!({"text":"ZZZ"}));
        assert!(matches!(crate::control::handle(&mut h.state_mut().0, &ctx, &request), crate::control::Outcome::AfterInput));
        let events = h.state_mut().0.take_synthetic_step();
        assert!(!events.is_empty());
        h.input_mut().events.extend(events);
        h.run();
        let (app, id) = h.state_mut();
        assert_eq!(app.ui.dialog_mut(*id).unwrap().fields["choices"][DEFAULT_DISPLAY], "/outside/monitor.icc");
        app.automation_input = false;
        assert!(restricted(app, *id));
        assert!(apply(app, *id, false).unwrap_err().contains("Automated dialogs"));
        assert_eq!(app.session.color.settings.monitor_profile, "auto");
    }

    #[test]
    fn pending_native_file_answer_cannot_edit_a_dialog_restricted_in_the_meantime() {
        use egui_kittest::{Harness, kittest::Queryable};
        let mut app = app();
        let (service, asked) = crate::file_dialog::fake(vec![Some(crate::file_dialog::FileDialogAnswer::Paths(vec!["/outside/monitor.icc".into()]))]);
        app.services.file_dialog = Some(service);
        let id = open(&mut app);
        let mut h = Harness::builder().with_size(egui::vec2(640.0, 500.0)).build_ui_state(draw_test_dialog, (app, id));
        PhotocraftApp::setup_context(&h.ctx, Default::default());
        h.run_steps(3);
        h.get_by_label("Choose ICC File…").click();
        h.run();
        let (app, id) = h.state_mut();
        assert!(app.file_dialog.is_some());
        let result = control(app, "ui.dialog.set", json!({"dialog":id,"field":"target","value":"default"}));
        assert_eq!(result["ok"], true, "{result}");
        app.poll_file_dialog(&egui::Context::default(), None);
        assert_eq!(asked.borrow().len(), 1);
        assert!(app.ui.status.contains("Automated dialogs"), "{}", app.ui.status);
        assert_eq!(app.ui.dialog_mut(*id).unwrap().fields["choices"][DEFAULT_DISPLAY], "auto");
    }

    #[test]
    fn parent_color_settings_cannot_replay_its_old_monitor_choice() {
        let mut app = app();
        let ctx = egui::Context::default();
        let parent = crate::prefs_ui::invoke(&mut app, &ctx, "edit.colorSettings", &json!({})).unwrap().unwrap()["dialog"].as_u64().unwrap();
        let id = open(&mut app);
        choose(&mut app, id, "display-p3");
        apply(&mut app, id, true).unwrap();
        crate::dialogs::confirm(&mut app, parent).unwrap();
        assert_eq!(app.session.color.settings.monitor_profile, "display-p3");
    }

    #[test]
    fn parent_color_settings_cannot_replay_its_old_per_display_choice() {
        let mut app = app();
        let key = photocraft_engine::monitor_cmds::mac_display_key(1, 2, 3).unwrap();
        let _ = app.session.color.set_displays(Ok(vec![photocraft_engine::display_color::Display {
            id: 7,
            key: Some(key.clone()),
            name: "Test monitor".into(),
            frame: [0.0, 0.0, 1920.0, 1080.0],
            profile_name: None,
            icc: None,
        }]));
        app.session.execute(COMMAND, json!({"set":[{"display":key,"profile":"srgb"}]})).unwrap();
        let ctx = egui::Context::default();
        let parent = crate::prefs_ui::invoke(&mut app, &ctx, "edit.colorSettings", &json!({})).unwrap().unwrap()["dialog"].as_u64().unwrap();
        let id = open(&mut app);
        app.ui.dialog_mut(id).unwrap().fields.get_mut("choices").unwrap()[&key] = json!("display-p3");
        apply(&mut app, id, true).unwrap();
        crate::dialogs::confirm(&mut app, parent).unwrap();
        assert_eq!(app.session.color.settings.monitor_overrides[&key], "display-p3");
    }
}
