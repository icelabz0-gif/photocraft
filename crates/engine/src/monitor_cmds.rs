//! Per-display monitor choices. File reads happen in commands/jobs, never while drawing.
//! `monitorProfile` remains the legacy default; overrides are keyed by physical identity.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(not(target_arch = "wasm32"))]
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use photocraft_cms::{Builtin, ColorSpace, Profile, ProfileClass, Transform};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::color_cmds::ColorSettings;
use crate::commands::CommandSpec;
use crate::display_color::{DISPLAY_BPC, DISPLAY_INTENT, MonitorDetection};
use crate::{EngineError, Result, Session};

pub const COMMAND: &str = "edit.monitorProfiles";
pub const DEFAULT_DISPLAY: &str = "default";
pub const MAX_RULES: usize = 64;
pub const MAX_PROFILE_BYTES: u64 = 32 << 20;
pub const MAX_TOTAL_BYTES: u64 = 64 << 20;
const MAX_SPEC: usize = 4096;
const MAX_KEY: usize = 128;
/// IOKit `kDisplayVendorIDUnknown` ('unkn').
const UNKNOWN_VENDOR: u32 = 0x756e_6b6e;

/// No serial, or a sentinel, is not a persistent identity. Do not substitute a port or name.
pub fn mac_display_key(vendor: u32, model: u32, serial: u32) -> Option<String> {
    if matches!(vendor, 0 | UNKNOWN_VENDOR | u32::MAX) || matches!(serial, 0 | u32::MAX) {
        return None;
    }
    let mut hash = blake3::Hasher::new();
    for part in [vendor, model, serial] {
        hash.update(&part.to_be_bytes());
    }
    Some(format!("mac-cg1:{}", hash.finalize().to_hex()))
}

#[derive(Clone)]
pub struct LoadedProfile {
    pub profile: Arc<Profile>,
    pub fingerprint: String,
    pub bytes: u64,
}

#[derive(Clone, Default)]
pub struct Runtime {
    pub(crate) files: BTreeMap<String, std::result::Result<LoadedProfile, String>>,
    pub(crate) revision: u64,
}

impl Runtime {
    pub(crate) fn install(&mut self, settings: &ColorSettings, loaded: BTreeMap<String, LoadedProfile>) {
        for (spec, profile) in loaded {
            self.files.insert(spec, Ok(profile));
        }
        let needed = needed_files(settings);
        self.files.retain(|key, _| needed.contains(key));
        self.changed();
    }

    pub(crate) fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
}

pub fn automatic(spec: &str) -> bool {
    spec.is_empty() || spec == "auto"
}

/// An explicit grammar: working/document aliases are deliberately not monitor destinations.
pub fn builtin(spec: &str) -> Option<Builtin> {
    Builtin::from_id(spec)
}

pub fn validate_profile(profile: &Profile) -> std::result::Result<(), String> {
    if profile.color_space != ColorSpace::Rgb {
        return Err(format!("`{}` is {:?}, not RGB", profile.description, profile.color_space));
    }
    if !matches!(profile.class, ProfileClass::Display | ProfileClass::ColorSpace) {
        return Err(format!("`{}` can't be used as a display profile (expected display or RGB color-space class)", profile.description));
    }
    Transform::new(Builtin::Srgb.profile(), profile, DISPLAY_INTENT, DISPLAY_BPC)
        .map_err(|e| format!("`{}` can't be used as a display profile: {e}", profile.description))?;
    Ok(())
}

pub fn fingerprint(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Read a regular, bounded ICC file. Nonblocking open also closes the Unix FIFO replacement
/// race between the path check and open; all operations use safe Rust APIs.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_profile_bytes(path: &Path) -> std::result::Result<Vec<u8>, String> {
    read_profile_bytes_limited(path, &mut (MAX_PROFILE_BYTES + 1))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn read_profile_bytes_limited(path: &Path, remaining: &mut u64) -> std::result::Result<Vec<u8>, String> {
    let fail = |e: &dyn std::fmt::Display| format!("cannot read profile `{}`: {e}", path.display());
    let metadata = std::fs::metadata(path).map_err(|e| fail(&e))?;
    if !metadata.is_file() || metadata.len() > MAX_PROFILE_BYTES || metadata.len() > *remaining {
        return Err(fail(&"expected a regular ICC file of at most 32 MiB"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| fail(&e))?;
    let metadata = file.metadata().map_err(|e| fail(&e))?;
    if !metadata.is_file() || metadata.len() > MAX_PROFILE_BYTES || metadata.len() > *remaining {
        return Err(fail(&"expected a regular ICC file of at most 32 MiB"));
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(metadata.len() as usize).map_err(|e| fail(&e))?;
    let limit = (MAX_PROFILE_BYTES + 1).min(*remaining);
    let result = file.take(limit).read_to_end(&mut bytes);
    *remaining = remaining.saturating_sub(bytes.len() as u64);
    result.map_err(|e| fail(&e))?;
    // Reaching the limit cannot prove EOF. Refuse instead of accepting a truncated profile.
    if bytes.len() as u64 == limit {
        return Err(fail(&"ICC read budget exhausted"));
    }
    if bytes.len() as u64 > MAX_PROFILE_BYTES || bytes.len() < 132 || bytes.get(36..40) != Some(b"acsp") {
        return Err(fail(&"invalid or oversized ICC profile"));
    }
    Ok(bytes)
}

#[cfg(target_arch = "wasm32")]
pub fn read_profile_bytes(_path: &Path) -> std::result::Result<Vec<u8>, String> {
    Err("ICC file paths are unavailable on this platform".into())
}

pub fn load_profile(spec: &str) -> std::result::Result<LoadedProfile, String> {
    load_profile_limited(spec, &mut (MAX_PROFILE_BYTES + 1))
}

fn load_profile_limited(spec: &str, remaining: &mut u64) -> std::result::Result<LoadedProfile, String> {
    if spec.is_empty() || spec.len() > MAX_SPEC || spec.contains('\0') {
        return Err("invalid monitor profile specification".into());
    }
    let (profile, bytes) = if let Some(b) = builtin(spec) {
        (Arc::new(b.profile().clone()), b.profile().to_bytes())
    } else {
        let extension = Path::new(spec).extension().and_then(|s| s.to_str()).unwrap_or("");
        if !extension.eq_ignore_ascii_case("icc") && !extension.eq_ignore_ascii_case("icm") {
            return Err("choose Automatic, a built-in RGB profile or an .icc/.icm file".into());
        }
        #[cfg(not(target_arch = "wasm32"))]
        let bytes = Arc::new(read_profile_bytes_limited(Path::new(spec), remaining)?);
        #[cfg(target_arch = "wasm32")]
        let bytes = {
            let _ = remaining;
            Arc::new(read_profile_bytes(Path::new(spec))?)
        };
        let profile = crate::color_cmds::profile_from_bytes(&bytes).map_err(|e| e.to_string())?;
        (profile, bytes)
    };
    validate_profile(&profile)?;
    Ok(LoadedProfile { profile, fingerprint: fingerprint(&bytes), bytes: bytes.len() as u64 })
}

/// Only sanitize the newly introduced field on load. One damaged rule must not reset all
/// working spaces; command writes remain strict.
pub(crate) fn sanitize_saved(value: &mut Value) {
    let Some(object) = value.as_object_mut() else { return };
    if let Some(rules) = object.get_mut("monitorOverrides") {
        let clean: BTreeMap<String, String> = rules
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(key, value)| value.as_str().filter(|spec| valid_rule(key, spec)).map(|spec| (key.clone(), spec.into())))
            .take(MAX_RULES)
            .collect();
        *rules = json!(clean);
    }
}

fn valid_rule(key: &str, spec: &str) -> bool {
    !key.is_empty()
        && key != DEFAULT_DISPLAY
        && key.len() <= MAX_KEY
        && !key.contains('\0')
        && !spec.is_empty()
        && spec.len() <= MAX_SPEC
        && !spec.contains('\0')
}

pub(crate) fn validate_rules(settings: &ColorSettings) -> std::result::Result<(), String> {
    if settings.monitor_overrides.len() > MAX_RULES || settings.monitor_overrides.iter().any(|(k, v)| !valid_rule(k, v)) {
        return Err("monitor overrides need at most 64 bounded display keys and profile specifications".into());
    }
    Ok(())
}

pub fn status(s: &Session) -> Value {
    json!({"default": s.color.settings.monitor_profile, "overrides": s.color.settings.monitor_overrides,
        "displays": s.color.display_statuses(), "monitorStatus": s.color.monitor_status()})
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Set {
    display: String,
    profile: String,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Request {
    set: Vec<Set>,
    clear: Vec<String>,
    refresh: bool,
}

fn bad(message: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: COMMAND.into(), msg: message.into() }
}

fn target(s: &Session, key: &str) -> Result<()> {
    if key == DEFAULT_DISPLAY {
        return Ok(());
    }
    if key.len() > MAX_KEY || key.is_empty() {
        return Err(bad("invalid display key"));
    }
    if !matches!(s.color.monitor_detection, MonitorDetection::Found) {
        return Err(bad("refresh the display list before changing a per-display profile"));
    }
    if s.color.displays.iter().filter(|d| d.key.as_deref() == Some(key)).count() != 1 {
        return Err(bad("the display is disconnected or its persistent identity is ambiguous"));
    }
    Ok(())
}

fn same_rules(a: &ColorSettings, b: &ColorSettings) -> bool {
    a.monitor_profile == b.monitor_profile && a.monitor_overrides == b.monitor_overrides
}

pub(crate) fn needed_files(settings: &ColorSettings) -> BTreeSet<String> {
    std::iter::once(&settings.monitor_profile).chain(settings.monitor_overrides.values()).filter(|s| !automatic(s) && builtin(s).is_none()).cloned().collect()
}

fn command(s: &mut Session, params: &Value) -> Result<Value> {
    let request: Request = serde_json::from_value(params.clone()).map_err(|e| bad(e.to_string()))?;
    if request.set.len().saturating_add(request.clear.len()) > MAX_RULES + 1 {
        return Err(bad("too many monitor changes"));
    }
    if request.refresh {
        if !request.set.is_empty() || !request.clear.is_empty() {
            return Err(bad("refresh cannot be combined with changes"));
        }
        return refresh(s);
    }
    if request.set.is_empty() && request.clear.is_empty() {
        return Ok(status(s));
    }
    let previous = s.color.settings.clone();
    let mut next = previous.clone();
    let mut targets = BTreeSet::new();
    for change in &request.set {
        target(s, &change.display)?;
        if !targets.insert(change.display.clone()) {
            return Err(bad("duplicate display change"));
        }
        if change.profile.is_empty() || change.profile.len() > MAX_SPEC {
            return Err(bad("invalid profile specification"));
        }
        if change.display == DEFAULT_DISPLAY {
            next.monitor_profile = change.profile.clone();
        } else {
            next.monitor_overrides.insert(change.display.clone(), change.profile.clone());
        }
    }
    for key in &request.clear {
        if !valid_rule(key, "auto") || !targets.insert(key.clone()) {
            return Err(bad("invalid or duplicate display key to clear"));
        }
        next.monitor_overrides.remove(key);
    }
    validate_rules(&next).map_err(bad)?;
    let revision = s.color.monitor_files.revision;
    let retained_bytes: u64 = s
        .color
        .monitor_files
        .files
        .iter()
        .filter(|(key, _)| needed_files(&next).contains(*key) && !request.set.iter().any(|c| &c.profile == *key))
        .filter_map(|(_, p)| p.as_ref().ok())
        .map(|p| p.bytes)
        .sum();
    let changes = request.set;
    let specs: BTreeSet<String> = changes.iter().map(|c| c.profile.clone()).filter(|spec| !automatic(spec)).collect();
    crate::jobs::run(
        s,
        "Applying monitor profiles",
        false,
        move |ctx| {
            let mut loaded = BTreeMap::new();
            let mut total = retained_bytes;
            let mut remaining = MAX_TOTAL_BYTES.saturating_sub(retained_bytes);
            for spec in specs {
                ctx.check()?;
                let profile = load_profile_limited(&spec, &mut remaining).map_err(bad)?;
                total = total.saturating_add(profile.bytes);
                if total > MAX_TOTAL_BYTES {
                    return Err(bad("monitor profile changes exceed 64 MiB"));
                }
                loaded.insert(spec, profile);
            }
            Ok(loaded)
        },
        move |s, loaded| {
            if !same_rules(&s.color.settings, &previous) || s.color.monitor_files.revision != revision {
                return Err(bad("monitor settings changed while profiles were being read; try again"));
            }
            for change in &changes {
                target(s, &change.display)?;
            }
            s.color.settings.monitor_profile = next.monitor_profile;
            s.color.settings.monitor_overrides = next.monitor_overrides;
            for (spec, profile) in loaded {
                s.color.monitor_files.files.insert(spec, Ok(profile));
            }
            let needed = needed_files(&s.color.settings);
            s.color.monitor_files.files.retain(|key, _| needed.contains(key));
            s.color.monitor_files.changed();
            s.prefs.edit(|_| ());
            Ok(status(s))
        },
    )
}

fn refresh(s: &mut Session) -> Result<Value> {
    let settings = s.color.settings.clone();
    let revision = s.color.monitor_files.revision;
    let specs = needed_files(&settings);
    crate::jobs::run(
        s,
        "Reading monitor profiles",
        false,
        move |ctx| {
            let mut loaded = BTreeMap::new();
            let mut total = 0u64;
            let mut remaining = MAX_TOTAL_BYTES;
            for spec in specs {
                ctx.check()?;
                let profile = if total >= MAX_TOTAL_BYTES {
                    Err("saved monitor profiles exceed 64 MiB".into())
                } else {
                    load_profile_limited(&spec, &mut remaining).and_then(|p| {
                        total = total.saturating_add(p.bytes);
                        if total > MAX_TOTAL_BYTES { Err("saved monitor profiles exceed 64 MiB".into()) } else { Ok(p) }
                    })
                };
                loaded.insert(spec, profile);
            }
            Ok(loaded)
        },
        move |s, loaded| {
            if !same_rules(&s.color.settings, &settings) || s.color.monitor_files.revision != revision {
                return Ok(json!({"stale": true}));
            }
            s.color.monitor_files.files = loaded;
            s.color.monitor_files.changed();
            Ok(status(s))
        },
    )
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: COMMAND,
        label: "Monitor Profiles…",
        menu: &[],
        shortcut: None,
        params: r##"{"set":[{"display":"default|persistent display key","profile":"auto|built-in RGB id|.icc/.icm path"}]?,"clear":["persistent display key"]?,"refresh":bool=false} (empty reads memory-only status; set is atomic; clear restores inheritance; refresh reloads saved files. Per-display set requires a currently connected unique identity; CLI without displays can set default only.)"##,
        enabled: crate::commands::always,
        run: command,
        journal: false,
    }]
}

/// Compatibility setters validate only changed monitor rules. An unplugged/deleted saved
/// profile must not prevent changing a working space.
pub(crate) fn changed_files(previous: &ColorSettings, next: &ColorSettings, runtime: &Runtime) -> std::result::Result<BTreeMap<String, LoadedProfile>, String> {
    validate_rules(next)?;
    let old = needed_files(previous);
    let mut loaded = BTreeMap::new();
    let needed = needed_files(next);
    let mut total: u64 = runtime.files.iter().filter(|(key, _)| needed.contains(*key)).filter_map(|(_, p)| p.as_ref().ok()).map(|p| p.bytes).sum();
    let mut remaining = MAX_TOTAL_BYTES.saturating_sub(total);
    for spec in needed.difference(&old) {
        let profile = load_profile_limited(spec, &mut remaining)?;
        total = total.saturating_add(profile.bytes);
        if total > MAX_TOTAL_BYTES {
            return Err("monitor profiles exceed 64 MiB".into());
        }
        loaded.insert(spec.clone(), profile);
    }
    for spec in std::iter::once(&next.monitor_profile).chain(next.monitor_overrides.values()) {
        if !automatic(spec) && builtin(spec).is_some() {
            load_profile(spec)?;
        }
    }
    Ok(loaded)
}

/// A shell transaction can restore just monitor choices if preference persistence fails.
/// Other color settings and document state are never reverted.
pub struct Snapshot {
    default: String,
    overrides: BTreeMap<String, String>,
    runtime: Runtime,
}
impl Snapshot {
    pub fn capture(s: &Session) -> Self {
        Self {
            default: s.color.settings.monitor_profile.clone(),
            overrides: s.color.settings.monitor_overrides.clone(),
            runtime: s.color.monitor_files.clone(),
        }
    }
    pub fn restore(self, s: &mut Session) {
        s.color.settings.monitor_profile = self.default;
        s.color.settings.monitor_overrides = self.overrides;
        let revision = s.color.monitor_files.revision.wrapping_add(1);
        s.color.monitor_files = self.runtime;
        s.color.monitor_files.revision = revision;
        s.prefs.edit(|_| ());
    }
}

#[cfg(test)]
#[path = "monitor_cmds_tests.rs"]
mod tests;
