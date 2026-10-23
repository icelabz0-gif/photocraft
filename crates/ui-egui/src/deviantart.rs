//! File › Submit to DeviantArt…. Uploads to Sta.sh, DeviantArt's drafts, not the public gallery.
//!
//! The refresh token stays in the system keychain. `deviantart.json` keeps the app id, title,
//! tags and the auto-submit switches. The web build has no submit.

use serde_json::{Value, json};

use crate::PhotocraftApp;

mod errors;
mod format;
mod pending;
#[cfg(test)]
mod tests;

pub use format::{clean_tags, item_id, multipart, stash_url};

/// Sta.sh accepts smaller files than Telegram. Larger ones fail with a validation error.
#[cfg(not(target_arch = "wasm32"))]
const MAX_BYTES: u64 = 30 * 1024 * 1024;
const BASE_SCOPES: &str = "stash gallery browse";
const ALL_SCOPES: &str = "stash gallery browse publish user.manage";

/// Loopback the user must whitelist on their DeviantArt app. A fixed port so the redirect URI
/// does not change between logins.
pub const REDIRECT_URI: &str = "http://127.0.0.1:43821/callback";

pub const DEVELOPER_PAGE: &str = "https://www.deviantart.com/developers/";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteKind {
    Save,
    SaveAs,
    SaveCopy,
    Export,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub client_id: String,
    pub refresh_token: String,
    pub scopes: String,
    pub title: String,
    pub tags: String,
    pub send_on_save: bool,
    pub send_on_save_as: bool,
    pub send_on_export: bool,
    pub send_on_save_copy: bool,
    pub last_item_id: String,
    /// Sta.sh uploads from this app, newest first. DeviantArt no longer offers a list endpoint.
    pub recent: Vec<RecentItem>,
}

/// One Sta.sh draft this app uploaded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecentItem {
    pub item_id: i64,
    pub title: String,
    pub url: String,
}

impl Settings {
    fn ready(&self) -> bool {
        !self.client_id.is_empty() && !self.refresh_token.is_empty()
    }

    fn wants(&self, kind: WriteKind) -> bool {
        match kind {
            WriteKind::Save => self.send_on_save,
            WriteKind::SaveAs => self.send_on_save_as,
            WriteKind::SaveCopy => self.send_on_save_copy,
            WriteKind::Export => self.send_on_export,
        }
    }
}

pub fn settings_from_json(text: &str) -> Settings {
    let Ok(value) = serde_json::from_str::<Value>(text) else { return Settings::default() };
    let flag = |key: &str| value.get(key).and_then(Value::as_bool).unwrap_or(false);
    let text_of = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    Settings {
        client_id: text_of("clientId"),
        refresh_token: text_of("refreshToken"),
        scopes: value
            .get("scopes")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| if text_of("refreshToken").is_empty() { String::new() } else { ALL_SCOPES.into() }),
        title: text_of("title"),
        tags: text_of("tags"),
        send_on_save: flag("sendOnSave"),
        send_on_save_as: flag("sendOnSaveAs"),
        send_on_export: flag("sendOnExport"),
        send_on_save_copy: flag("sendOnSaveCopy"),
        last_item_id: text_of("lastItemId"),
        recent: value
            .get("recentItems")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let item_id = item.get("itemId").and_then(Value::as_i64)?;
                        let url = item.get("url").and_then(Value::as_str)?.to_string();
                        let title = item.get("title").and_then(Value::as_str).unwrap_or("Sta.sh").to_string();
                        Some(RecentItem { item_id, title, url })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

pub fn settings_json(s: &Settings) -> String {
    serde_json::to_string(&json!({
        "clientId": s.client_id,
        "refreshToken": s.refresh_token,
        "scopes": s.scopes,
        "title": s.title,
        "tags": s.tags,
        "sendOnSave": s.send_on_save,
        "sendOnSaveAs": s.send_on_save_as,
        "sendOnExport": s.send_on_export,
        "sendOnSaveCopy": s.send_on_save_copy,
        "lastItemId": s.last_item_id,
        "recentItems": s.recent.iter().map(|item| json!({"itemId": item.item_id, "title": item.title, "url": item.url})).collect::<Vec<_>>(),
    }))
    .unwrap_or_else(|_| "{}".to_string())
}

pub fn without_token(text: &str) -> Result<(String, String), String> {
    let mut value: Value = serde_json::from_str(text).unwrap_or_else(|_| json!({}));
    let token = value.get("refreshToken").and_then(Value::as_str).unwrap_or("").to_string();
    if let Some(object) = value.as_object_mut() {
        object.remove("refreshToken");
    }
    Ok((serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string()), token))
}

pub fn with_token(text: &str, token: &str) -> String {
    let mut value: Value = serde_json::from_str(text).unwrap_or_else(|_| json!({}));
    if let Some(object) = value.as_object_mut() {
        object.insert("refreshToken".into(), json!(token));
    }
    serde_json::to_string(&value).unwrap_or_else(|_| text.to_string())
}

pub fn command(app: &mut PhotocraftApp) -> Result<Value, String> {
    let pending = app.session.active().and_then(|d| d.path.clone());
    Ok(json!({"dialog": open_dialog(app, pending)}))
}

pub fn after_written(app: &mut PhotocraftApp, path: &str, kind: WriteKind) {
    if !app.deviantart.wants(kind) {
        return;
    }
    if !app.deviantart.ready() {
        pending::enqueue(app, path);
        if !dialog_open(app) {
            let id = open_dialog(app, None);
            if let Some(dialog) = app.ui.dialog_mut(id) {
                dialog.fields.insert("__autoPending".into(), json!(true));
            }
        }
        return;
    }
    if app.deviantart_job.is_some() {
        pending::enqueue(app, path);
        return;
    }
    if let Err(error) = start_submit(app, path.to_string(), false) {
        app.ui.status = error;
        app.ui.status_error = true;
    }
}

pub fn arm_after_save(app: &mut PhotocraftApp, path: &str, kind: WriteKind, result: &Value) {
    if result.get("pending").and_then(Value::as_bool) == Some(true) {
        if let Some(save) = app.jobs.saves.iter_mut().rev().find(|save| save.path == path && !save.ended) {
            save.notify_deviantart = Some(kind);
        }
        return;
    }
    after_written(app, path, kind);
}

pub(crate) fn busy(app: &PhotocraftApp) -> bool {
    app.deviantart_job.is_some() || (app.deviantart.ready() && !app.deviantart_pending.is_empty())
}

pub fn poll(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(job) = app.deviantart_job.as_ref() else {
        pending::start_next(app, ctx);
        return;
    };
    let finished = match job.rx.try_recv() {
        Ok(result) => Some(result),
        Err(std::sync::mpsc::TryRecvError::Empty) => {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err("desktop".to_string())),
    };
    let Some(job) = app.deviantart_job.take() else { return };
    if job.temporary {
        remove_temp_path(&job.path);
    }
    match (job.kind, finished) {
        (JobKind::Connect, Some(Ok(token))) => {
            app.deviantart.refresh_token = token;
            app.deviantart.scopes = job.name.clone();
            if let Some(dialog) = app.ui.dialogs.iter_mut().find(|dialog| owns(&dialog.fields)) {
                dialog.fields.insert("connected".into(), json!(true));
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            match save_settings(app) {
                Ok(()) => {
                    app.ui.status = tl!("DeviantArt is connected.").to_string();
                    app.ui.status_error = false;
                }
                Err(error) => {
                    app.ui.status = error;
                    app.ui.status_error = true;
                }
            }
        }
        (_, Some(Ok(body))) if body.starts_with("OPEN\t") => {
            errors::remember_rotated_login(app);
            let path = body.trim_start_matches("OPEN\t");
            let opened = std::fs::read(path).ok();
            let name = file_name(path);
            remove_temp_path(path);
            if let Some(bytes) = opened {
                if let Err(error) = app.open_bytes(&name, &bytes) {
                    app.ui.status = error;
                    app.ui.status_error = true;
                } else {
                    app.ui.status = crate::i18n::fmt(tl!("Opened {name}."), &[("name", &name)]);
                    app.ui.status_error = false;
                }
            } else {
                app.ui.status = tl!("Could not open the DeviantArt download.").to_string();
                app.ui.status_error = true;
            }
        }
        (JobKind::Submit, Some(Ok(body))) => {
            // DeviantArt invalidates the refresh token it just accepted and returns a new one.
            // Pick that up before saving, or the next call sends the dead token.
            errors::remember_rotated_login(app);
            let id = item_id(&body).map(|id| id.to_string()).unwrap_or_default();
            let url = item_id(&body).and_then(stash_url);
            if !id.is_empty() {
                app.deviantart.last_item_id = id.clone();
                if let Some(url) = &url {
                    remember_stash(app, id.parse().unwrap_or(0), &job.name, url);
                }
                if let Some(dialog) = app.ui.dialogs.iter_mut().find(|dialog| owns(&dialog.fields)) {
                    dialog.fields.insert("itemId".into(), json!(id));
                    dialog.fields.insert("stash".into(), json!(stash_rows_from(&app.deviantart)));
                }
                let _ = save_settings(app);
            }
            let message = crate::i18n::fmt(tl!("Submitted {name} to Sta.sh. It stays private until you publish it."), &[("name", &job.name)]);
            app.ui.status = match &url {
                Some(url) => format!("{message} {url}"),
                None => message.clone(),
            };
            app.ui.status_error = false;
            show_result(app, message, url);
        }
        (JobKind::Publish, Some(Ok(_))) => {
            errors::remember_rotated_login(app);
            let message = tl!("Published on DeviantArt.").to_string();
            app.ui.status = message.clone();
            app.ui.status_error = false;
            show_result(app, message, None);
        }
        (JobKind::Edit, Some(Ok(_))) => {
            errors::remember_rotated_login(app);
            let message = tl!("Updated the title and tags on DeviantArt.").to_string();
            app.ui.status = message.clone();
            app.ui.status_error = false;
            show_result(app, message, None);
        }
        (JobKind::Avatar, Some(Ok(_))) => {
            errors::remember_rotated_login(app);
            let message = tl!("Set the DeviantArt profile picture.").to_string();
            app.ui.status = message.clone();
            app.ui.status_error = false;
            show_result(app, message, None);
        }
        (JobKind::Gallery, Some(Ok(body))) => {
            errors::remember_rotated_login(app);
            if let Some(dialog) = app.ui.dialogs.iter_mut().find(|dialog| owns(&dialog.fields)) {
                let rows = gallery_rows(&body);
                if rows.is_empty() {
                    dialog.fields.insert("galleryNote".into(), json!(tl!("No published works in the gallery.").to_string()));
                } else {
                    dialog.fields.remove("galleryNote");
                }
                dialog.fields.insert("gallery".into(), json!(rows));
                dialog.fields.remove("filesError");
            }
            app.ui.status_error = false;
        }
        (JobKind::Gallery, Some(Err(error))) => {
            if errors::login_was_rejected(&error) {
                app.deviantart.refresh_token.clear();
                let _ = save_settings(app);
            } else {
                errors::remember_rotated_login(app);
            }
            let message = errors::localize_error(&error);
            if let Some(dialog) = app.ui.dialogs.iter_mut().find(|dialog| owns(&dialog.fields)) {
                dialog.fields.insert("filesError".into(), json!(message.clone()));
            }
            app.ui.status = message;
            app.ui.status_error = true;
        }
        (JobKind::Download, Some(Err(error)))
        | (JobKind::Publish, Some(Err(error)))
        | (JobKind::Edit, Some(Err(error)))
        | (JobKind::Avatar, Some(Err(error))) => {
            errors::report_deviantart_error(app, &error, true);
        }
        (_, Some(Err(error))) => {
            errors::report_deviantart_error(app, &error, !matches!(job.kind, JobKind::Connect));
        }
        _ => {}
    }
    pending::start_next(app, ctx);
    crate::jobs_ui::close_after_saves(app, ctx);
}

pub fn owns(fields: &serde_json::Map<String, Value>) -> bool {
    fields.get("__deviantart").and_then(Value::as_bool) == Some(true)
}

fn dialog_open(app: &PhotocraftApp) -> bool {
    app.ui.dialogs.iter().any(|d| owns(&d.fields))
}

fn open_dialog(app: &mut PhotocraftApp, pending: Option<String>) -> u64 {
    let mut fields = serde_json::Map::new();
    fields.insert("__deviantart".into(), json!(true));
    fields.insert("__label".into(), json!("Submit to DeviantArt"));
    fields.insert("clientId".into(), json!(app.deviantart.client_id));
    fields.insert("title".into(), json!(app.deviantart.title));
    fields.insert("tags".into(), json!(app.deviantart.tags));
    fields.insert("connected".into(), json!(app.deviantart.ready()));
    fields.insert("onSave".into(), json!(app.deviantart.send_on_save));
    fields.insert("onSaveAs".into(), json!(app.deviantart.send_on_save_as));
    fields.insert("onExport".into(), json!(app.deviantart.send_on_export));
    fields.insert("onSaveCopy".into(), json!(app.deviantart.send_on_save_copy));
    let saved = pending.as_ref().is_some_and(|path| std::fs::metadata(path).is_ok_and(|meta| meta.is_file()))
        || app.session.active().and_then(|d| d.path.as_deref()).is_some_and(|path| std::fs::metadata(path).is_ok_and(|meta| meta.is_file()));
    fields.insert("format".into(), json!(if saved { "file" } else { "png" }));
    if let Some(path) = pending {
        fields.insert("pending".into(), json!(path));
    }
    app.ui.open_dialog(crate::state::DialogKind::Command, fields)
}

pub fn body(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut serde_json::Map<String, Value>) {
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.set_min_width(440.0);
    ui.label(egui::RichText::new(tl!("Submitting needs an internet connection.")).color(t.text_dim));
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(tl!(
            "Create a Public app on DeviantArt, without a client secret. Copy the address below into the redirect field, paste the client id here, then connect."
        ))
        .color(t.text_dim),
    );
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.monospace(REDIRECT_URI);
        if crate::widgets::secondary_button(ui, tl!("Copy"), 72.0).clicked() {
            ui.ctx().copy_text(REDIRECT_URI.to_string());
            app.ui.status = tl!("Copied the redirect address.").to_string();
            app.ui.status_error = false;
        }
    });
    ui.add_space(4.0);
    if crate::widgets::secondary_button(ui, tl!("Open the DeviantArt developer page"), 260.0).clicked() {
        crate::links::open(app, ui.ctx(), DEVELOPER_PAGE);
    }
    ui.add_space(8.0);
    ui.label(tl!("DeviantArt app id"));
    let mut client_id = f.get("clientId").and_then(Value::as_str).unwrap_or("").to_string();
    if ui.add(egui::TextEdit::singleline(&mut client_id).desired_width(ui.available_width())).changed() {
        f.insert("clientId".into(), json!(client_id.clone()));
    }
    ui.label(egui::RichText::new(tl!("The DeviantArt login stays in the system keychain, not in PhotoCraft's settings file.")).color(t.text_dim));
    ui.add_space(4.0);
    let connected = app.deviantart.ready();
    if connected {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tl!("Connected")).strong());
            if crate::widgets::secondary_button(ui, tl!("Connect again"), 150.0).clicked() {
                app.deviantart.client_id = client_id.clone();
                if let Err(error) = start_connect(app, "") {
                    app.ui.status = error;
                    app.ui.status_error = true;
                }
            }
            ui.add_enabled_ui(app.deviantart_job.is_none(), |ui| {
                if crate::widgets::secondary_button(ui, tl!("Disconnect DeviantArt"), 170.0).clicked() {
                    errors::disconnect(app);
                }
            });
        });
    } else if crate::widgets::secondary_button(ui, tl!("Connect DeviantArt"), 220.0).clicked() {
        app.deviantart.client_id = client_id;
        if let Err(error) = start_connect(app, "") {
            app.ui.status = error;
            app.ui.status_error = true;
        }
    }
    ui.add_space(8.0);
    ui.label(tl!("Title"));
    let mut title = f.get("title").and_then(Value::as_str).unwrap_or("").to_string();
    if ui.add(egui::TextEdit::singleline(&mut title).desired_width(ui.available_width())).changed() {
        f.insert("title".into(), json!(title));
    }
    ui.label(tl!("Tags"));
    let mut tags = f.get("tags").and_then(Value::as_str).unwrap_or("").to_string();
    if ui.add(egui::TextEdit::singleline(&mut tags).desired_width(ui.available_width())).changed() {
        f.insert("tags".into(), json!(tags));
    }
    ui.label(egui::RichText::new(tl!("Letters, numbers and underscores, separated by commas.")).color(t.text_dim));
    ui.add_space(8.0);
    ui.label(tl!("Submit as"));
    let format = f.get("format").and_then(Value::as_str).unwrap_or("psd").to_string();
    ui.horizontal(|ui| {
        for (id, label) in [("file", tl!("Saved file")), ("psd", "PSD"), ("png", "PNG")] {
            if ui.selectable_label(format == id, label).clicked() {
                f.insert("format".into(), json!(id));
            }
        }
    });
    ui.label(egui::RichText::new(tl!("PNG is what DeviantArt shows. A PSD is only stored as a file in Sta.sh.")).color(t.text_dim));
    ui.label(egui::RichText::new(tl!("OK uploads to Sta.sh. Nothing is public until you publish.")).color(t.text_dim));
    ui.add_space(8.0);
    gallery_section(app, ui, f);
    ui.add_space(8.0);
    let mut more = f.get("more").and_then(Value::as_bool).unwrap_or(false);
    if crate::widgets::checkbox(ui, &mut more, tl!("More options")).changed() {
        f.insert("more".into(), json!(more));
    }
    if !more {
        return;
    }
    ui.add_space(4.0);
    ui.label(egui::RichText::new(tl!("Connect again after this update so gallery, publish and the profile picture are allowed.")).color(t.text_dim));
    ui.label(tl!("Sta.sh item id"));
    let mut item_id = f.get("itemId").and_then(Value::as_str).unwrap_or("").to_string();
    if item_id.is_empty() {
        item_id = app.deviantart.last_item_id.clone();
    }
    if ui.add(egui::TextEdit::singleline(&mut item_id).desired_width(120.0)).changed() {
        f.insert("itemId".into(), json!(item_id.clone()));
    }
    let mut public_ok = f.get("publicOk").and_then(Value::as_bool).unwrap_or(false);
    if crate::widgets::checkbox(ui, &mut public_ok, tl!("Publish this publicly")).changed() {
        f.insert("publicOk".into(), json!(public_ok));
    }
    let mut mature = f.get("mature").and_then(Value::as_bool).unwrap_or(false);
    if crate::widgets::checkbox(ui, &mut mature, tl!("Not for all ages")).changed() {
        f.insert("mature".into(), json!(mature));
    }
    ui.label(tl!("License"));
    let mut creative_commons = f.get("creativeCommons").and_then(Value::as_bool).unwrap_or(false);
    if crate::widgets::checkbox(ui, &mut creative_commons, tl!("Creative Commons")).changed() {
        f.insert("creativeCommons".into(), json!(creative_commons));
    }
    let mut commercial = f.get("commercial").and_then(Value::as_bool).unwrap_or(false);
    if crate::widgets::checkbox(ui, &mut commercial, tl!("Commercial use")).changed() {
        f.insert("commercial".into(), json!(commercial));
    }
    if crate::widgets::secondary_button(ui, tl!("Publish"), 160.0).clicked() {
        if !public_ok {
            app.ui.status = tl!("Confirm that this publish is public.").to_string();
            app.ui.status_error = true;
        } else if item_id.trim().is_empty() || item_id == "0" {
            app.ui.status = tl!("Upload with OK first. Publish uses the Sta.sh item from that upload.").to_string();
            app.ui.status_error = true;
        } else if !app.deviantart.scopes.split_whitespace().any(|scope| scope == "publish") {
            app.ui.status = tl!("Reconnect to DeviantArt to grant permission to publish.").to_string();
            if let Err(error) = start_connect(app, "publish") {
                app.ui.status = error;
                app.ui.status_error = true;
            }
        } else if let Err(error) = start_call(
            app,
            &json!({"op":"publish","itemId": item_id.parse::<i64>().unwrap_or(0), "mature": mature, "level": "moderate", "classes": [], "creativeCommons": creative_commons, "commercial": commercial, "modify": "no"}).to_string(),
        ) {
            app.ui.status = error;
            app.ui.status_error = true;
        }
    }
    ui.label(tl!("Deviation id"));
    let mut deviation_id = f.get("deviationId").and_then(Value::as_str).unwrap_or("").to_string();
    if ui.add(egui::TextEdit::singleline(&mut deviation_id).desired_width(ui.available_width())).changed() {
        f.insert("deviationId".into(), json!(deviation_id.clone()));
    }
    if crate::widgets::secondary_button(ui, tl!("Update title and tags"), 220.0).clicked() {
        let tags = clean_tags(f.get("tags").and_then(Value::as_str).unwrap_or(""));
        let title = f.get("title").and_then(Value::as_str).unwrap_or("");
        if let Err(error) = start_call(app, &json!({"op":"edit","deviationId": deviation_id, "title": title, "tags": tags}).to_string()) {
            app.ui.status = error;
            app.ui.status_error = true;
        }
    }
    ui.label(tl!("Profile picture"));
    let avatar = f.get("avatar").and_then(Value::as_str).unwrap_or("png").to_string();
    ui.horizontal(|ui| {
        for (id, label) in [("png", "PNG"), ("jpg", "JPEG")] {
            if ui.selectable_label(avatar == id, label).clicked() {
                f.insert("avatar".into(), json!(id));
            }
        }
    });
    if crate::widgets::secondary_button(ui, tl!("Set as DeviantArt profile picture"), 260.0).clicked() {
        if !app.deviantart.scopes.split_whitespace().any(|scope| scope == "user.manage") {
            app.ui.status = tl!("Reconnect to DeviantArt to grant permission to change your profile picture.").to_string();
            if let Err(error) = start_connect(app, "user.manage") {
                app.ui.status = error;
                app.ui.status_error = true;
            }
            return;
        }
        match avatar_file(app, &avatar) {
            Ok(path) => {
                if let Err(error) = start_call(app, &json!({"op":"avatar","path": path}).to_string()) {
                    app.ui.status = error;
                    app.ui.status_error = true;
                }
            }
            Err(error) => {
                app.ui.status = error;
                app.ui.status_error = true;
            }
        }
    }
    ui.add_space(8.0);
    crate::widgets::section_label(ui, "Submit automatically");
    ui.add_space(4.0);
    for (key, label) in [
        ("onSave", tl!("Also submit when I Save")),
        ("onSaveAs", tl!("Also submit when I Save As")),
        ("onSaveCopy", tl!("Also submit when I Save a Copy")),
        ("onExport", tl!("Also submit when I Export")),
    ] {
        let mut on = f.get(key).and_then(Value::as_bool).unwrap_or(false);
        if crate::widgets::checkbox(ui, &mut on, label).changed() {
            f.insert(key.into(), json!(on));
        }
    }
}

pub fn confirm(app: &mut PhotocraftApp, f: &serde_json::Map<String, Value>) -> Result<Value, String> {
    apply_fields(app, f)?;
    save_settings(app)?;
    if f.get("__autoPending").and_then(Value::as_bool) == Some(true) {
        return Ok(Value::Null);
    }
    let format = f.get("format").and_then(Value::as_str).unwrap_or("psd");
    let pending = f.get("pending").and_then(Value::as_str).map(str::to_string);
    if app.session.active().is_none() && pending.is_none() {
        app.ui.status = tl!("DeviantArt is set up. Save or export a file, then submit it.").to_string();
        app.ui.status_error = false;
        return Ok(Value::Null);
    }
    if !app.deviantart.ready() {
        return Err(tl!("Connect DeviantArt first.").to_string());
    }
    let (path, temporary) = resolve_payload(app, format, pending)?;
    start_submit(app, path, temporary)?;
    Ok(Value::Null)
}

fn apply_fields(app: &mut PhotocraftApp, f: &serde_json::Map<String, Value>) -> Result<(), String> {
    let client_id = f.get("clientId").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if !client_id.is_empty() && !client_id.chars().all(|c| c.is_ascii_digit()) {
        return Err(tl!("The DeviantArt app id should be a number.").to_string());
    }
    app.deviantart.client_id = client_id;
    app.deviantart.title = f.get("title").and_then(Value::as_str).unwrap_or("").trim().to_string();
    app.deviantart.tags = f.get("tags").and_then(Value::as_str).unwrap_or("").trim().to_string();
    app.deviantart.send_on_save = f.get("onSave").and_then(Value::as_bool).unwrap_or(false);
    app.deviantart.send_on_save_as = f.get("onSaveAs").and_then(Value::as_bool).unwrap_or(false);
    app.deviantart.send_on_export = f.get("onExport").and_then(Value::as_bool).unwrap_or(false);
    app.deviantart.send_on_save_copy = f.get("onSaveCopy").and_then(Value::as_bool).unwrap_or(false);
    Ok(())
}

fn save_settings(app: &mut PhotocraftApp) -> Result<(), String> {
    let text = settings_json(&app.deviantart);
    let Some(save) = app.services.save_deviantart.as_mut() else {
        return Err(tl!("DeviantArt submitting needs the desktop app.").to_string());
    };
    save(&text)
}

#[derive(Clone, Copy)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))] // Desktop-only workers cannot be constructed in the web build.
enum JobKind {
    Connect,
    Submit,
    Publish,
    Edit,
    Avatar,
    Gallery,
    Download,
}

pub(crate) struct Job {
    rx: std::sync::mpsc::Receiver<Result<String, String>>,
    kind: JobKind,
    path: String,
    temporary: bool,
    name: String,
}

fn start_connect(app: &mut PhotocraftApp, additional_scopes: &str) -> Result<(), String> {
    if app.deviantart_job.is_some() {
        return Err(tl!("A DeviantArt submit is already running.").to_string());
    }
    let client_id = app.deviantart.client_id.trim().to_string();
    if client_id.is_empty() || !client_id.chars().all(|c| c.is_ascii_digit()) {
        return Err(tl!("The DeviantArt app id should be a number.").to_string());
    }
    let Some(connect) = app.services.deviantart_connect.clone() else {
        return Err(tl!("DeviantArt submitting needs the desktop app.").to_string());
    };
    let scopes = requested_scopes(&app.deviantart.scopes, &format!("{BASE_SCOPES} {additional_scopes}"));
    let _scopes_for_job = scopes.clone();
    let (tx, _rx) = std::sync::mpsc::channel::<Result<String, String>>();
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (connect, client_id, tx);
        Err(tl!("DeviantArt submitting needs the desktop app.").to_string())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::Builder::new()
            .name("pc-da-login".into())
            .spawn(move || {
                let _ = tx.send(connect(&client_id, &scopes));
            })
            .map_err(|error| error.to_string())?;
        app.ui.status = tl!("Waiting for DeviantArt…").to_string();
        app.ui.status_error = false;
        app.deviantart_job = Some(Job { rx: _rx, kind: JobKind::Connect, path: String::new(), temporary: false, name: _scopes_for_job });
        Ok(())
    }
}

fn requested_scopes(granted: &str, needed: &str) -> String {
    let mut scopes = Vec::new();
    for scope in granted.split_whitespace().chain(needed.split_whitespace()) {
        if matches!(scope, "stash" | "gallery" | "browse" | "publish" | "user.manage") && !scopes.contains(&scope) {
            scopes.push(scope);
        }
    }
    scopes.join(" ")
}

fn start_submit(app: &mut PhotocraftApp, path: String, temporary: bool) -> Result<(), String> {
    if app.deviantart_job.is_some() {
        if temporary {
            remove_temp_path(&path);
        }
        return Err(tl!("A DeviantArt submit is already running.").to_string());
    }
    let Some(submit) = app.services.deviantart_submit.clone() else {
        if temporary {
            remove_temp_path(&path);
        }
        return Err(tl!("DeviantArt submitting needs the desktop app.").to_string());
    };
    let (name, bytes) = match read_file(&path) {
        Ok(read) => read,
        Err(error) => {
            if temporary {
                remove_temp_path(&path);
            }
            return Err(error);
        }
    };
    let title = if app.deviantart.title.is_empty() { stem(&name) } else { app.deviantart.title.clone() };
    let comments = comments(&name, bytes.len() as u64);
    let tags = clean_tags(&app.deviantart.tags);
    let token = app.deviantart.refresh_token.clone();
    let client_id = app.deviantart.client_id.clone();
    let (tx, _rx) = std::sync::mpsc::channel::<Result<String, String>>();
    let file_name = name.clone();
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (submit, token, client_id, title, comments, tags, file_name, bytes, tx);
        Err(tl!("DeviantArt submitting needs the desktop app.").to_string())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::Builder::new()
            .name("pc-da-submit".into())
            .spawn(move || {
                let result = submit(&token, &client_id, &title, &comments, &tags, &file_name, &bytes);
                let _ = tx.send(result);
            })
            .map_err(|error| error.to_string())?;
        app.ui.status = crate::i18n::fmt(tl!("Submitting {name} to DeviantArt…"), &[("name", &name)]);
        app.ui.status_error = false;
        app.deviantart_job = Some(Job { rx: _rx, kind: JobKind::Submit, path, temporary, name });
        Ok(())
    }
}

fn resolve_payload(app: &mut PhotocraftApp, format: &str, pending: Option<String>) -> Result<(String, bool), String> {
    match format {
        "png" => export_temp(app, "png").map(|path| (path, true)),
        "psd" => export_temp(app, "psd").map(|path| (path, true)),
        _ => saved_file(app, pending).ok_or_else(|| tl!("There is no saved file to send. Choose PSD or PNG.").to_string()),
    }
}

fn saved_file(app: &PhotocraftApp, pending: Option<String>) -> Option<(String, bool)> {
    let path = pending
        .filter(|path| std::fs::metadata(path).is_ok_and(|meta| meta.is_file()))
        .or_else(|| app.session.active().and_then(|doc| doc.path.clone()).filter(|path| std::fs::metadata(path).is_ok_and(|meta| meta.is_file())))?;
    Some((path, false))
}

fn export_temp(app: &mut PhotocraftApp, extension: &str) -> Result<String, String> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (app, extension);
        return Err(tl!("DeviantArt submitting needs the desktop app.").to_string());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let doc = app.session.active().ok_or_else(|| "no document".to_string())?.doc.clone();
        let export = app.services.export.as_ref().ok_or_else(|| tl!("DeviantArt submitting needs the desktop app.").to_string())?;
        let stem = stem(&file_name(&doc.name));
        let temp_dir = tempfile::Builder::new().prefix("photocraft-deviantart-").tempdir().map_err(|error| error.to_string())?;
        let path = temp_dir.path().join(format!("{stem}.{extension}"));
        let path_str = path.display().to_string();
        let settings = crate::ExportSettings { xmp_all: false, tiff_layers: false, ..crate::ExportSettings::default() };
        let (bytes, _) =
            export(&doc, &path_str, &settings).map_err(|error| crate::i18n::fmt(tl!("Could not prepare a file to send: {error}"), &[("error", &error)]))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(tl!("That file is larger than DeviantArt accepts (30 MB).").to_string());
        }
        std::fs::write(&path, &bytes).map_err(|error| error.to_string())?;
        let _ = temp_dir.keep();
        Ok(path_str)
    }
}

fn remove_temp_path(path: &str) {
    let path = std::path::Path::new(path);
    let _ = std::fs::remove_file(path);
    if let Some(parent) =
        path.parent().filter(|parent| parent.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("photocraft-deviantart-")))
    {
        let _ = std::fs::remove_dir(parent);
    }
}

fn read_file(path: &str) -> Result<(String, Vec<u8>), String> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = path;
        return Err(tl!("DeviantArt submitting needs the desktop app.").to_string());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Err(tl!("There is no saved file to send. Choose PSD or PNG.").to_string());
        }
        if meta.len() > MAX_BYTES {
            return Err(tl!("That file is larger than DeviantArt accepts (30 MB).").to_string());
        }
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        Ok((file_name(path), bytes))
    }
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string()
}

fn stem(name: &str) -> String {
    let stem = std::path::Path::new(name).file_stem().and_then(|n| n.to_str()).unwrap_or(name);
    let stem: String = stem.chars().take(50).collect();
    if stem.is_empty() { "PhotoCraft".to_string() } else { stem }
}

fn comments(name: &str, bytes: u64) -> String {
    let format = std::path::Path::new(name).extension().and_then(|ext| ext.to_str()).unwrap_or("").to_ascii_uppercase();
    format!("File: {name}\nFormat: {format}\nSize: {}", crate::sizing::human_bytes(bytes as f64))
}

pub fn gallery_rows(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let Ok(value) = serde_json::from_str::<Value>(text) else { return out };
    let Some(results) = value.get("results").and_then(Value::as_array) else { return out };
    for item in results {
        let Some(id) = item.get("deviationid").and_then(Value::as_str) else { continue };
        if id.is_empty() {
            continue;
        }
        let title = item.get("title").and_then(Value::as_str).unwrap_or(id);
        out.push(json!({"id": id, "title": title}));
    }
    out
}

fn start_call(app: &mut PhotocraftApp, request: &str) -> Result<(), String> {
    let parsed: Value = serde_json::from_str(request).unwrap_or_else(|_| json!({}));
    let op = parsed.get("op").and_then(Value::as_str).unwrap_or("");
    let (_kind, path, temporary) = match op {
        "publish" => (JobKind::Publish, String::new(), false),
        "edit" => (JobKind::Edit, String::new(), false),
        "gallery" => (JobKind::Gallery, String::new(), false),
        "download" => (JobKind::Download, String::new(), false),
        "avatar" => {
            let path = parsed.get("path").and_then(Value::as_str).unwrap_or("").to_string();
            (JobKind::Avatar, path, true)
        }
        _ => (JobKind::Submit, String::new(), false),
    };
    let discard_temp = |path: &str| {
        if temporary {
            remove_temp_path(path);
        }
    };
    if app.deviantart_job.is_some() {
        discard_temp(&path);
        return Err(tl!("A DeviantArt submit is already running.").to_string());
    }
    let Some(call) = app.services.deviantart_call.clone() else {
        discard_temp(&path);
        return Err(tl!("DeviantArt submitting needs the desktop app.").to_string());
    };
    let refresh = app.deviantart.refresh_token.clone();
    let client_id = app.deviantart.client_id.clone();
    let request = request.to_string();
    let (tx, _rx) = std::sync::mpsc::channel::<Result<String, String>>();
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (call, refresh, client_id, request, tx);
        discard_temp(&path);
        Err(tl!("DeviantArt submitting needs the desktop app.").to_string())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::Builder::new()
            .name("pc-da-call".into())
            .spawn(move || {
                let _ = tx.send(call(&refresh, &client_id, &request));
            })
            .map_err(|error| {
                discard_temp(&path);
                error.to_string()
            })?;
        app.deviantart_job = Some(Job { rx: _rx, kind: _kind, path, temporary, name: "DeviantArt".into() });
        Ok(())
    }
}

fn avatar_file(app: &mut PhotocraftApp, format: &str) -> Result<String, String> {
    let extension = if format == "jpg" { "jpg" } else { "png" };
    export_temp(app, extension)
}

fn remember_stash(app: &mut PhotocraftApp, item_id: i64, title: &str, url: &str) {
    if item_id <= 0 {
        return;
    }
    app.deviantart.recent.retain(|item| item.item_id != item_id);
    app.deviantart.recent.insert(0, RecentItem { item_id, title: if title.is_empty() { "Sta.sh".into() } else { title.into() }, url: url.to_string() });
    app.deviantart.recent.truncate(10);
}

fn stash_rows_from(settings: &Settings) -> Vec<Value> {
    let mut items = settings.recent.clone();
    if items.is_empty()
        && let Ok(item_id) = settings.last_item_id.parse::<i64>()
        && let Some(url) = stash_url(item_id)
    {
        let title = if settings.title.is_empty() { "Sta.sh".into() } else { settings.title.clone() };
        items.push(RecentItem { item_id, title, url });
    }
    items.into_iter().map(|item| json!({"itemId": item.item_id, "title": item.title, "url": item.url})).collect()
}

fn gallery_section(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut serde_json::Map<String, Value>) {
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(tl!("Sta.sh drafts open their page. Published works open in PhotoCraft.")).color(t.text_dim));
    ui.add_space(4.0);
    if f.get("stash").is_none() {
        f.insert("stash".into(), json!(stash_rows_from(&app.deviantart)));
    }
    if crate::widgets::secondary_button(ui, tl!("Get files"), 160.0).clicked() {
        f.remove("filesError");
        f.remove("galleryNote");
        let stash = stash_rows_from(&app.deviantart);
        if stash.is_empty() {
            f.insert("filesError".into(), json!(tl!("No Sta.sh files yet.").to_string()));
        }
        f.insert("stash".into(), json!(stash));
        if let Err(error) = start_call(app, r#"{"op":"gallery"}"#) {
            f.insert("filesError".into(), json!(error));
        }
    }
    if let Some(error) = f.get("filesError").and_then(Value::as_str) {
        ui.label(egui::RichText::new(error).color(t.warning));
    }
    if let Some(note) = f.get("galleryNote").and_then(Value::as_str) {
        ui.label(note);
    }
    if let Some(rows) = f.get("stash").and_then(Value::as_array).cloned() {
        for row in rows {
            let item_id = row.get("itemId").and_then(Value::as_i64).unwrap_or(0);
            let label = row.get("title").and_then(Value::as_str).unwrap_or("Sta.sh").to_string();
            if item_id <= 0 {
                continue;
            }
            let page = row.get("url").and_then(Value::as_str).unwrap_or("").to_string();
            if ui.selectable_label(false, label).clicked() {
                if page.is_empty() {
                    app.ui.status = tl!("DeviantArt did not offer a download.").to_string();
                    app.ui.status_error = true;
                } else {
                    crate::links::open(app, ui.ctx(), &page);
                }
            }
        }
    }
    if let Some(rows) = f.get("gallery").and_then(Value::as_array).cloned() {
        for row in rows {
            let id = row.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            let label = row.get("title").and_then(Value::as_str).unwrap_or(&id).to_string();
            if id.is_empty() {
                continue;
            }
            if ui.selectable_label(false, label).clicked()
                && let Err(error) = start_call(app, &json!({"op":"download","deviationId": id}).to_string())
            {
                app.ui.status = error;
                app.ui.status_error = true;
            }
        }
    }
}

pub fn result_owns(fields: &serde_json::Map<String, Value>) -> bool {
    fields.get("__deviantart_result").and_then(Value::as_bool) == Some(true)
}

pub fn result_body(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &serde_json::Map<String, Value>) {
    ui.set_min_width(420.0);
    if let Some(message) = f.get("message").and_then(Value::as_str) {
        ui.label(message);
        ui.add_space(8.0);
    }
    let Some(url) = f.get("url").and_then(Value::as_str) else { return };
    ui.hyperlink(url);
    ui.add_space(4.0);
    if crate::widgets::secondary_button(ui, tl!("Open the Sta.sh page"), 200.0).clicked() {
        crate::links::open(app, ui.ctx(), url);
    }
}

fn show_result(app: &mut PhotocraftApp, message: String, url: Option<String>) {
    let mut fields = serde_json::Map::new();
    fields.insert("__deviantart_result".into(), json!(true));
    fields.insert("__label".into(), json!("DeviantArt"));
    fields.insert("message".into(), json!(message));
    if let Some(url) = url {
        fields.insert("url".into(), json!(url));
    }
    app.ui.open_dialog(crate::state::DialogKind::Command, fields);
}
