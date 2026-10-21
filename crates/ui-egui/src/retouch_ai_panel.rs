//! Window › Retouch AI: the floating panel that drives the `retouchAI.*` engine commands.
//!
//! The panel is deliberately local only (no cloud, no network): one engine row says so. It holds
//! the 14 retouch tools in three collapsible groups, each with a checkbox and an amount slider,
//! a preset row (four built-in looks plus "Custom", and user presets saved in the UI layout), the
//! analysis/combo status line, the layers a run created with live opacity and visibility, the
//! reference image for Color Match, and a batch runner that calls the existing
//! `file.automate.batch` command with one `retouchAI.run` step.
//!
//! All state lives in [`RetouchAiState`], part of [`crate::state::UiState`], so presets and the
//! panel's choices survive a relaunch and automation can read/drive them. The panel never touches
//! the document directly: every action is an engine command (`retouchAI.analyze`,
//! `retouchAI.run`, `layer.setProps`, `file.automate.batch`).

use egui::{Align2, CornerRadius, Sense, Stroke, vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::file_dialog::{FileDialogAnswer, FileDialogRequest};
use crate::theme::{self, Tokens};
use crate::{PhotocraftApp, icons, widgets};

/// Panel width, in points, like Brush Settings' fixed card.
pub const WIDTH: f32 = 352.0;

/// `last_preset` / `preset_choice` value for "Custom" (the current tools, not a stored preset).
pub const CUSTOM: &str = "custom";

/// The panel's English source keys, as data rather than literal `tl!` calls.
///
/// Retouch AI ships Vietnamese first: every key below has a `vi.tsv` row (checked by
/// `tests::vietnamese_covers_every_panel_key`), while the other complete catalogs fall back to
/// English for this new panel until a translator covers it. Keeping the keys out of the literal
/// scanner is how the rest of the shell handles labels that aren't complete in every language yet
/// (see `brush_panel::SECTIONS`, `camera_raw_ui`'s dynamic titles).
mod text {
    // Window chrome and the action row.
    pub const TITLE: &str = "Retouch AI";
    pub const ENGINE: &str = "Engine";
    pub const LOCAL_CPU: &str = "Local • CPU";
    pub const CLOSE: &str = "Close";
    pub const OPEN_IMAGE_TO_START: &str = "Open an image to start";
    pub const ANALYZE: &str = "Analyze";
    pub const RUN_SELECTED: &str = "Run Selected";

    // Presets.
    pub const PRESET: &str = "Preset";
    pub const CUSTOM: &str = "Custom";
    pub const SAVE: &str = "Save";
    pub const DELETE: &str = "Delete";
    pub const PRESET_NAME: &str = "Preset name";
    pub const PRESET_NAME_EMPTY: &str = "Preset name is empty.";
    pub const PRESET_SAVED: &str = "Preset saved: {name}";
    pub const PRESET_DELETED: &str = "Preset deleted: {name}";
    pub const BUILTIN_ID_PHOTO: &str = "ID Photo";
    pub const BUILTIN_OUTDOOR_WEDDING: &str = "Outdoor Wedding";
    pub const BUILTIN_BEAUTY_STUDIO: &str = "Beauty Studio";
    pub const BUILTIN_CHILDREN_YEARBOOK: &str = "Children's Yearbook";

    // Color Match's reference image.
    pub const REFERENCE_IMAGE: &str = "Reference image";
    pub const CHOOSE_REFERENCE: &str = "Choose Reference Image…";
    pub const NO_REFERENCE: &str = "No reference chosen";
    pub const REFERENCE_SET: &str = "Reference image: {name}";
    pub const CHOOSE_REFERENCE_FAILED: &str = "Choose the reference image: {error}";

    // Analysis and run status.
    pub const YES: &str = "Yes";
    pub const NO: &str = "No";
    pub const ANALYSIS_COUNTS: &str = "Faces: {faces} · Eyes: {eyes} · Mouth: {mouth} · Skin: {skin}";
    pub const ANALYSIS_FAILED: &str = "Analysis failed: {error}";
    pub const RETOUCH_FAILED: &str = "Retouch failed: {error}";
    pub const CREATED_LAYERS: &str = "Created {n} layers";
    pub const SKIPPED: &str = "Skipped: {tool} ({reason})";
    pub const RETOUCH_LAYER_NAME: &str = "Retouch • {tool}";
    pub const SELECT_A_TOOL: &str = "Select at least one tool.";
    pub const LAYER_UPDATE_FAILED: &str = "Layer update failed: {error}";

    // Results.
    pub const RESULTS: &str = "Results";
    pub const NO_RESULTS: &str = "No results yet.";
    pub const SHOW_LAYER: &str = "Show layer";
    pub const HIDE_LAYER: &str = "Hide layer";

    // Batch.
    pub const BATCH: &str = "Batch";
    pub const INPUT_FOLDER: &str = "Input folder";
    pub const OUTPUT_FOLDER: &str = "Output folder";
    pub const FORMAT: &str = "Format";
    pub const FOLDER_PATH: &str = "Folder path";
    pub const BROWSE: &str = "Browse…";
    pub const RUN_BATCH: &str = "Run Batch";
    pub const SAME_AS_SOURCE: &str = "Same as source";
    pub const JPEG: &str = "JPEG";
    pub const PNG: &str = "PNG";
    pub const TIFF: &str = "TIFF";
    pub const CHOOSE_FOLDERS: &str = "Choose both an input and an output folder.";
    pub const BATCH_FINISHED: &str = "Batch finished: {files} files, {errors} errors";
    pub const BATCH_FAILED: &str = "Batch failed: {error}";

    // Tool labels.
    pub const TOOL_HEAL: &str = "Heal";
    pub const TOOL_DODGE_BURN: &str = "Dodge & Burn";
    pub const TOOL_SKIN_TONE: &str = "Skin Tone";
    pub const TOOL_MATTIFIER: &str = "Mattifier";
    pub const TOOL_SKIN_MASK: &str = "Skin Mask";
    pub const TOOL_PORTRAIT_VOLUMES: &str = "Portrait Volumes";
    pub const TOOL_EYE_BRILLIANCE: &str = "Eye Brilliance";
    pub const TOOL_EYE_VESSELS: &str = "Eye Vessels";
    pub const TOOL_TEETH: &str = "Teeth Whitening";
    pub const TOOL_CLEAN_HAIR: &str = "Clean Hair";
    pub const TOOL_FABRIC_VOLUMES: &str = "Fabric Volumes";
    pub const TOOL_DUST_CLEAN: &str = "Dust Clean";
    pub const TOOL_BACKGROUND_CLEAN: &str = "Background Clean";
    pub const TOOL_COLOR_MATCH: &str = "Color Match";

    // Tool hints (tooltips).
    pub const HINT_HEAL: &str = "Removes blemishes, spots and redness in the skin area while keeping texture.";
    pub const HINT_DODGE_BURN: &str = "Dodges and burns the skin to soften under-eye shadows and smile lines.";
    pub const HINT_SKIN_TONE: &str = "Evens out the skin tone and reduces redness or color casts.";
    pub const HINT_MATTIFIER: &str = "Reduces oily shine and blown highlights on the skin.";
    pub const HINT_SKIN_MASK: &str = "Creates a copy of the layer with a skin mask; the pixels are unchanged.";
    pub const HINT_PORTRAIT_VOLUMES: &str = "Adds depth and volume to the subject.";
    pub const HINT_EYE_BRILLIANCE: &str = "Adds contrast to the iris and brightens the catchlights.";
    pub const HINT_EYE_VESSELS: &str = "Reduces red blood vessels in the whites of the eyes.";
    pub const HINT_TEETH: &str = "Whitens the teeth naturally and reduces yellow stains.";
    pub const HINT_CLEAN_HAIR: &str = "Removes flyaway hair around the edge of the head.";
    pub const HINT_FABRIC_VOLUMES: &str = "Dodges and burns folds in the fabric of the clothing.";
    pub const HINT_DUST_CLEAN: &str = "Removes dust and lint from dark backgrounds and clothing.";
    pub const HINT_BACKGROUND_CLEAN: &str = "Removes marks and creases from the backdrop.";
    pub const HINT_COLOR_MATCH: &str = "Matches the color tone to a reference image.";

    /// Every fixed key above, for `tests::vietnamese_covers_every_panel_key`. Tool, group,
    /// built-in and format keys are checked through their source functions instead.
    #[cfg(test)]
    pub const ALL: &[&str] = &[
        TITLE,
        ENGINE,
        LOCAL_CPU,
        CLOSE,
        OPEN_IMAGE_TO_START,
        ANALYZE,
        RUN_SELECTED,
        PRESET,
        CUSTOM,
        SAVE,
        DELETE,
        PRESET_NAME,
        PRESET_NAME_EMPTY,
        PRESET_SAVED,
        PRESET_DELETED,
        REFERENCE_IMAGE,
        CHOOSE_REFERENCE,
        NO_REFERENCE,
        REFERENCE_SET,
        CHOOSE_REFERENCE_FAILED,
        YES,
        NO,
        ANALYSIS_COUNTS,
        ANALYSIS_FAILED,
        RETOUCH_FAILED,
        CREATED_LAYERS,
        SKIPPED,
        RETOUCH_LAYER_NAME,
        SELECT_A_TOOL,
        LAYER_UPDATE_FAILED,
        RESULTS,
        NO_RESULTS,
        SHOW_LAYER,
        HIDE_LAYER,
        BATCH,
        INPUT_FOLDER,
        OUTPUT_FOLDER,
        FORMAT,
        FOLDER_PATH,
        BROWSE,
        RUN_BATCH,
        SAME_AS_SOURCE,
        JPEG,
        PNG,
        TIFF,
        CHOOSE_FOLDERS,
        BATCH_FINISHED,
        BATCH_FAILED,
    ];
}

/// A tool row: its engine id and the group it belongs to (index into [`TOOL_GROUPS`]).
pub struct ToolSpec {
    pub id: &'static str,
    pub group: usize,
}

/// The 14 retouch tools, in the panel's order (the contract's three groups).
pub const TOOLS: [ToolSpec; 14] = [
    ToolSpec { id: "heal", group: 0 },
    ToolSpec { id: "dodgeBurn", group: 0 },
    ToolSpec { id: "skinTone", group: 0 },
    ToolSpec { id: "mattifier", group: 0 },
    ToolSpec { id: "skinMask", group: 0 },
    ToolSpec { id: "portraitVolumes", group: 0 },
    ToolSpec { id: "eyeBrilliance", group: 1 },
    ToolSpec { id: "eyeVessels", group: 1 },
    ToolSpec { id: "teeth", group: 1 },
    ToolSpec { id: "cleanHair", group: 2 },
    ToolSpec { id: "fabricVolumes", group: 2 },
    ToolSpec { id: "dustClean", group: 2 },
    ToolSpec { id: "backgroundClean", group: 2 },
    ToolSpec { id: "colorMatch", group: 2 },
];

/// Group ids (stable keys for the collapsed list) and their English source labels.
pub const TOOL_GROUPS: [(&str, &str); 3] = [("skin", "Skin & Face"), ("eyes", "Eyes & Smile"), ("hair", "Hair, Fabric & Background")];

/// A tool's English source label; [`tool_label`] translates it.
fn tool_source(id: &str) -> &'static str {
    match id {
        "heal" => text::TOOL_HEAL,
        "dodgeBurn" => text::TOOL_DODGE_BURN,
        "skinTone" => text::TOOL_SKIN_TONE,
        "mattifier" => text::TOOL_MATTIFIER,
        "skinMask" => text::TOOL_SKIN_MASK,
        "portraitVolumes" => text::TOOL_PORTRAIT_VOLUMES,
        "eyeBrilliance" => text::TOOL_EYE_BRILLIANCE,
        "eyeVessels" => text::TOOL_EYE_VESSELS,
        "teeth" => text::TOOL_TEETH,
        "cleanHair" => text::TOOL_CLEAN_HAIR,
        "fabricVolumes" => text::TOOL_FABRIC_VOLUMES,
        "dustClean" => text::TOOL_DUST_CLEAN,
        "backgroundClean" => text::TOOL_BACKGROUND_CLEAN,
        "colorMatch" => text::TOOL_COLOR_MATCH,
        _ => "",
    }
}

/// A tool's label in the active UI language (the English source is the fallback).
pub fn tool_label(id: &str) -> &'static str {
    tl!(tool_source(id))
}

/// A tool's English tooltip; [`tool_hint`] translates it.
fn tool_hint_source(id: &str) -> &'static str {
    match id {
        "heal" => text::HINT_HEAL,
        "dodgeBurn" => text::HINT_DODGE_BURN,
        "skinTone" => text::HINT_SKIN_TONE,
        "mattifier" => text::HINT_MATTIFIER,
        "skinMask" => text::HINT_SKIN_MASK,
        "portraitVolumes" => text::HINT_PORTRAIT_VOLUMES,
        "eyeBrilliance" => text::HINT_EYE_BRILLIANCE,
        "eyeVessels" => text::HINT_EYE_VESSELS,
        "teeth" => text::HINT_TEETH,
        "cleanHair" => text::HINT_CLEAN_HAIR,
        "fabricVolumes" => text::HINT_FABRIC_VOLUMES,
        "dustClean" => text::HINT_DUST_CLEAN,
        "backgroundClean" => text::HINT_BACKGROUND_CLEAN,
        "colorMatch" => text::HINT_COLOR_MATCH,
        _ => "",
    }
}

/// A tool's hint in the active UI language (the English source is the fallback).
pub fn tool_hint(id: &str) -> &'static str {
    tl!(tool_hint_source(id))
}

/// A group's English source label; [`group_title`] translates it.
fn group_source(index: usize) -> &'static str {
    TOOL_GROUPS.get(index).map_or("", |(_, label)| *label)
}

fn group_title(index: usize) -> &'static str {
    tl!(group_source(index))
}

/// A built-in look: its stable key and the tools it turns on with their amounts.
pub struct BuiltinPreset {
    pub key: &'static str,
    pub tools: &'static [(&'static str, f32)],
}

/// The four built-in looks. Amounts are 0..100, like the sliders.
pub const BUILTINS: [BuiltinPreset; 4] = [
    BuiltinPreset {
        key: "idPhoto",
        tools: &[
            ("heal", 55.0),
            ("dodgeBurn", 50.0),
            ("skinTone", 60.0),
            ("mattifier", 45.0),
            ("portraitVolumes", 45.0),
            ("eyeBrilliance", 50.0),
            ("teeth", 55.0),
            ("cleanHair", 40.0),
            ("backgroundClean", 50.0),
        ],
    },
    BuiltinPreset {
        key: "outdoorWedding",
        tools: &[
            ("heal", 55.0),
            ("dodgeBurn", 60.0),
            ("skinTone", 60.0),
            ("mattifier", 50.0),
            ("portraitVolumes", 55.0),
            ("eyeBrilliance", 50.0),
            ("teeth", 55.0),
            ("cleanHair", 55.0),
            ("fabricVolumes", 55.0),
            ("backgroundClean", 50.0),
        ],
    },
    BuiltinPreset {
        key: "beautyStudio",
        tools: &[
            ("heal", 70.0),
            ("dodgeBurn", 70.0),
            ("skinTone", 70.0),
            ("mattifier", 65.0),
            ("skinMask", 100.0),
            ("portraitVolumes", 65.0),
            ("eyeBrilliance", 65.0),
            ("eyeVessels", 60.0),
            ("teeth", 70.0),
            ("cleanHair", 60.0),
        ],
    },
    BuiltinPreset {
        key: "childrenYearbook",
        tools: &[
            ("heal", 60.0),
            ("dodgeBurn", 50.0),
            ("skinTone", 60.0),
            ("mattifier", 50.0),
            ("portraitVolumes", 50.0),
            ("eyeBrilliance", 60.0),
            ("teeth", 55.0),
            ("cleanHair", 55.0),
            ("dustClean", 50.0),
            ("backgroundClean", 55.0),
        ],
    },
];

/// A built-in preset's English source label; [`builtin_label`] translates it.
fn builtin_source(key: &str) -> &'static str {
    match key {
        "idPhoto" => text::BUILTIN_ID_PHOTO,
        "outdoorWedding" => text::BUILTIN_OUTDOOR_WEDDING,
        "beautyStudio" => text::BUILTIN_BEAUTY_STUDIO,
        "childrenYearbook" => text::BUILTIN_CHILDREN_YEARBOOK,
        _ => "",
    }
}

/// A built-in preset's label in the active UI language (the English source is the fallback).
pub fn builtin_label(key: &str) -> &'static str {
    tl!(builtin_source(key))
}

/// One tool's selection and amount, as saved in the UI layout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RetouchToolState {
    pub id: String,
    pub enabled: bool,
    pub amount: f32,
}

impl Default for RetouchToolState {
    fn default() -> Self {
        Self { id: String::new(), enabled: false, amount: 60.0 }
    }
}

/// A user preset: a name and a snapshot of every tool's checkbox and amount.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RetouchPreset {
    pub name: String,
    pub tools: Vec<RetouchToolState>,
}

impl Default for RetouchPreset {
    fn default() -> Self {
        Self { name: String::new(), tools: default_tools() }
    }
}

/// A layer a `retouchAI.run` returned, kept per document for the Results list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RetouchResultLayer {
    pub tool: String,
    pub name: String,
    pub layer: u64,
    /// 0..100, what the layer's `layer.setProps` opacity is.
    pub opacity: f32,
    pub visible: bool,
}

impl Default for RetouchResultLayer {
    fn default() -> Self {
        Self { tool: String::new(), name: String::new(), layer: 0, opacity: 100.0, visible: true }
    }
}

/// Retouch AI panel state, persisted in [`crate::state::UiState`] so it survives a relaunch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RetouchAiState {
    /// Every tool, in [`TOOLS`] order (`ensure_tools` restores missing or duplicate ids).
    pub tools: Vec<RetouchToolState>,
    /// User presets (the four built-in looks live in [`BUILTINS`], never in this list).
    pub presets: Vec<RetouchPreset>,
    /// The selected preset: `custom`, `builtin:<key>` or `user:<name>`.
    pub last_preset: String,
    /// The Save field's text.
    pub preset_name: String,
    pub batch_input: String,
    pub batch_output: String,
    /// `same`, `jpg`, `png` or `tiff`.
    pub batch_format: String,
    /// Color Match's reference image (a path on the desktop, a name on the web).
    pub reference_path: String,
    /// The document index the result layers below belong to.
    pub result_document: Option<usize>,
    pub result_layers: Vec<RetouchResultLayer>,
    /// Collapsed group ids (and `results` / `batch`).
    pub collapsed: Vec<String>,
    /// The last status message: analysis counts, skipped tools, errors, presets.
    pub status: String,
    pub status_error: bool,
    /// The batch line, separate so a batch result doesn't hide the combo's analysis.
    pub batch_status: String,
    pub batch_status_error: bool,
    /// True while the (synchronous) batch call is in flight; the button is disabled meanwhile.
    #[serde(skip)]
    pub batch_running: bool,
}

impl Default for RetouchAiState {
    fn default() -> Self {
        Self {
            tools: default_tools(),
            presets: Vec::new(),
            last_preset: CUSTOM.into(),
            preset_name: String::new(),
            batch_input: String::new(),
            batch_output: String::new(),
            batch_format: "same".into(),
            reference_path: String::new(),
            result_document: None,
            result_layers: Vec::new(),
            collapsed: Vec::new(),
            status: String::new(),
            status_error: false,
            batch_status: String::new(),
            batch_status_error: false,
            batch_running: false,
        }
    }
}

fn default_enabled(id: &str) -> bool {
    matches!(id, "heal" | "dodgeBurn" | "skinTone" | "eyeBrilliance" | "teeth")
}

fn default_amount(id: &str) -> f32 {
    match id {
        "heal" => 70.0,
        "dodgeBurn" => 60.0,
        "skinTone" => 65.0,
        "mattifier" => 55.0,
        "skinMask" => 100.0,
        "portraitVolumes" => 60.0,
        "eyeBrilliance" => 60.0,
        "eyeVessels" => 55.0,
        "teeth" => 70.0,
        "cleanHair" => 60.0,
        "fabricVolumes" => 55.0,
        "dustClean" => 60.0,
        "backgroundClean" => 60.0,
        "colorMatch" => 50.0,
        _ => 60.0,
    }
}

/// The default 14 tool rows: Heal, Dodge & Burn, Skin Tone, Eye Brilliance and Teeth selected.
pub fn default_tools() -> Vec<RetouchToolState> {
    TOOLS.iter().map(|spec| RetouchToolState { id: spec.id.into(), enabled: default_enabled(spec.id), amount: default_amount(spec.id) }).collect()
}

fn clamp_amount(amount: f32) -> f32 {
    if amount.is_finite() { amount.clamp(0.0, 100.0) } else { 100.0 }
}

impl RetouchAiState {
    /// Every tool exactly once, in panel order, with finite amounts in 0..100. Call once per frame
    /// so a hand-edited or older layout can't hide a tool or produce a NaN slider.
    pub fn ensure_tools(&mut self) {
        let mut tools = Vec::with_capacity(TOOLS.len());
        for spec in TOOLS {
            let mut tool = self.tools.iter().find(|t| t.id == spec.id).cloned().unwrap_or(RetouchToolState {
                id: spec.id.into(),
                enabled: default_enabled(spec.id),
                amount: default_amount(spec.id),
            });
            tool.id = spec.id.into();
            tool.amount = clamp_amount(tool.amount);
            tools.push(tool);
        }
        self.tools = tools;
        self.batch_format = match self.batch_format.as_str() {
            "jpg" | "png" | "tiff" => self.batch_format.clone(),
            _ => "same".into(),
        };
    }

    pub fn tool(&self, id: &str) -> Option<&RetouchToolState> {
        self.tools.iter().find(|t| t.id == id)
    }

    pub fn tool_mut(&mut self, id: &str) -> Option<&mut RetouchToolState> {
        self.tools.iter_mut().find(|t| t.id == id)
    }

    /// Is the tool checked?
    pub fn enabled(&self, id: &str) -> bool {
        self.tool(id).is_some_and(|t| t.enabled)
    }

    /// The checked tools as `retouchAI.run`'s `tools` array.
    pub fn selected_tools(&self) -> Vec<Value> {
        self.tools.iter().filter(|t| t.enabled).map(|t| json!({"id": t.id, "amount": clamp_amount(t.amount)})).collect()
    }

    /// Replace every tool's selection and amount from a preset.
    pub fn apply_tools(&mut self, tools: &[RetouchToolState]) {
        for tool in &mut self.tools {
            tool.enabled = false;
        }
        for saved in tools {
            if let Some(tool) = self.tools.iter_mut().find(|t| t.id == saved.id) {
                tool.enabled = saved.enabled;
                tool.amount = clamp_amount(saved.amount);
            }
        }
    }

    /// Apply a built-in look by its key. False for an unknown key.
    pub fn apply_builtin(&mut self, key: &str) -> bool {
        let Some(preset) = BUILTINS.iter().find(|p| p.key == key) else { return false };
        for tool in &mut self.tools {
            tool.enabled = false;
        }
        for (id, amount) in preset.tools {
            if let Some(tool) = self.tools.iter_mut().find(|t| t.id == *id) {
                tool.enabled = true;
                tool.amount = clamp_amount(*amount);
            }
        }
        self.last_preset = format!("builtin:{key}");
        true
    }

    /// Apply a saved user preset by name. False when it isn't there.
    pub fn apply_user_preset(&mut self, name: &str) -> bool {
        let Some(preset) = self.presets.iter().find(|p| p.name == name) else { return false };
        let tools = preset.tools.clone();
        self.apply_tools(&tools);
        self.last_preset = format!("user:{name}");
        true
    }

    /// Save the current tools under `name`, replacing a preset with the same name.
    pub fn save_preset(&mut self, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(tl!(text::PRESET_NAME_EMPTY).into());
        }
        let preset = RetouchPreset { name: name.to_string(), tools: self.tools.clone() };
        match self.presets.iter_mut().find(|p| p.name == name) {
            Some(slot) => *slot = preset,
            None => self.presets.push(preset),
        }
        self.preset_name = name.to_string();
        self.last_preset = format!("user:{name}");
        Ok(())
    }

    /// Delete the selected user preset; `None` for a built-in or Custom selection.
    pub fn delete_selected_preset(&mut self) -> Option<String> {
        let name = self.last_preset.strip_prefix("user:")?.to_string();
        let index = self.presets.iter().position(|p| p.name == name)?;
        self.presets.remove(index);
        self.last_preset = CUSTOM.into();
        Some(name)
    }

    /// The combo box's text for the selected preset.
    pub fn preset_label(&self) -> String {
        if let Some(key) = self.last_preset.strip_prefix("builtin:") {
            return builtin_label(key).to_string();
        }
        if let Some(name) = self.last_preset.strip_prefix("user:") {
            return name.to_string();
        }
        tl!(text::CUSTOM).to_string()
    }
}

/// What the panel asks the app to do after it has drawn (so no UI-state borrow is held across
/// an engine call or a file dialog).
#[derive(Clone, Debug, PartialEq)]
enum Action {
    Analyze,
    Run,
    SetOpacity { layer: u64, opacity: f32, gesture: u64 },
    SetVisible { layer: u64, visible: bool },
    PickReference,
    PickBatchInput,
    PickBatchOutput,
    RunBatch,
}

/// A drag's coalesce number, so one opacity drag is one history step (the Brush Settings trick).
fn slider_gesture(ctx: &egui::Context) -> u64 {
    const KEY: &str = "retouch-ai-slider-gesture";
    let frame = ctx.cumulative_pass_nr();
    let pressed = ctx.input(|i| i.pointer.any_pressed());
    ctx.data_mut(|data| {
        let gesture = data.get_temp_mut_or_default::<(u64, u64)>(egui::Id::new(KEY));
        if pressed && gesture.1 != frame {
            gesture.0 += 1;
            gesture.1 = frame;
        }
        gesture.0
    })
}

fn set_status(state: &mut RetouchAiState, message: String, error: bool) {
    state.status = message;
    state.status_error = error;
}

fn set_batch_status(state: &mut RetouchAiState, message: String, error: bool) {
    state.batch_status = message;
    state.batch_status_error = error;
}

/// The file name of a path (the whole path when it has none), for the reference line.
fn file_name(path: &str) -> String {
    std::path::Path::new(path).file_name().and_then(|n| n.to_str()).map(str::to_string).unwrap_or_else(|| path.to_string())
}

/// The panel's analysis line: faces, eyes, mouth, skin. `value` is either the `retouchAI.analyze`
/// result or the `analysis` object of a run.
fn analysis_text(value: &Value) -> String {
    let faces = value.get("faces").and_then(Value::as_u64).unwrap_or(0);
    let eyes = value.get("eyes").and_then(Value::as_u64).unwrap_or(0);
    let yes_no = |key: &str| {
        if value.get(key).and_then(Value::as_bool).unwrap_or(false) {
            tl!(text::YES)
        } else {
            tl!(text::NO)
        }
    };
    crate::i18n::fmt(
        tl!(text::ANALYSIS_COUNTS),
        &[("faces", &faces.to_string()), ("eyes", &eyes.to_string()), ("mouth", yes_no("mouth")), ("skin", yes_no("skin"))],
    )
}

/// The run's status line: analysis, created layers and skipped tools with their reasons.
fn run_text(value: &Value, created: usize) -> String {
    let mut text = String::new();
    if let Some(analysis) = value.get("analysis") {
        text.push_str(&analysis_text(analysis));
        text.push('\n');
    }
    text.push_str(&crate::i18n::fmt(tl!(text::CREATED_LAYERS), &[("n", &created.to_string())]));
    if let Some(skipped) = value.get("skipped").and_then(Value::as_array) {
        for item in skipped {
            let tool = item.get("tool").and_then(Value::as_str).unwrap_or("");
            let name = tool_label(tool);
            let reason = item.get("reason").and_then(Value::as_str).unwrap_or("");
            text.push('\n');
            text.push_str(&crate::i18n::fmt(tl!(text::SKIPPED), &[("tool", name), ("reason", reason)]));
        }
    }
    text
}

/// Store the layers of a completed run for the active document; older result ids are replaced.
fn store_results(state: &mut RetouchAiState, value: &Value, document: Option<usize>) {
    let mut layers = Vec::new();
    if let Some(items) = value.get("layers").and_then(Value::as_array) {
        for item in items {
            let Some(layer) = item.get("layer").and_then(Value::as_u64) else { continue };
            let tool = item.get("tool").and_then(Value::as_str).unwrap_or("").to_string();
            let label = tool_label(&tool);
            let name = item.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| {
                crate::i18n::fmt(tl!(text::RETOUCH_LAYER_NAME), &[("tool", label)])
            });
            let opacity = item.get("opacity").and_then(Value::as_f64).map(|o| (o * 100.0) as f32).unwrap_or(100.0);
            layers.push(RetouchResultLayer { tool, name, layer, opacity: clamp_amount(opacity), visible: true });
        }
    }
    state.result_document = document;
    state.result_layers = layers;
}

/// Keep the Results list tied to its document: clear it on a switch, prune layers deleted since.
fn sync_results_to_document(app: &mut PhotocraftApp) {
    let active = app.session.active_index();
    let live: Option<Vec<u64>> =
        active.and_then(|i| app.session.documents().get(i)).map(|state| state.doc.layers.iter().map(|layer| layer.id.0).collect());
    let panel = &mut app.ui.retouch_ai;
    if panel.result_document != active {
        panel.result_layers.clear();
        panel.result_document = active;
    } else if let Some(live) = live {
        panel.result_layers.retain(|layer| live.contains(&layer.layer));
    }
}

fn analyze(app: &mut PhotocraftApp) {
    let result = app.run("retouchAI.analyze", json!({}));
    let state = &mut app.ui.retouch_ai;
    match result {
        Ok(value) => {
            let analysis = value.get("analysis").unwrap_or(&value);
            set_status(state, analysis_text(analysis), false);
        }
        Err(error) => set_status(state, crate::i18n::fmt(tl!(text::ANALYSIS_FAILED), &[("error", &error)]), true),
    }
}

fn run_combo(app: &mut PhotocraftApp) {
    let Some(document) = app.session.active_index() else {
        set_status(&mut app.ui.retouch_ai, tl!(text::OPEN_IMAGE_TO_START).into(), true);
        return;
    };
    let (tools, reference) = {
        let state = &app.ui.retouch_ai;
        (state.selected_tools(), state.reference_path.trim().to_string())
    };
    if tools.is_empty() {
        set_status(&mut app.ui.retouch_ai, tl!(text::SELECT_A_TOOL).into(), true);
        return;
    }
    let mut params = json!({"tools": tools, "sampleAllLayers": true});
    if !reference.is_empty() {
        params["referencePath"] = json!(reference);
    }
    match app.run("retouchAI.run", params) {
        Ok(value) => {
            let created = value.get("layers").and_then(Value::as_array).map_or(0, Vec::len);
            let text = run_text(&value, created);
            let state = &mut app.ui.retouch_ai;
            store_results(state, &value, Some(document));
            set_status(state, text, false);
        }
        Err(error) => set_status(&mut app.ui.retouch_ai, crate::i18n::fmt(tl!(text::RETOUCH_FAILED), &[("error", &error)]), true),
    }
}

fn run_batch(app: &mut PhotocraftApp) {
    let (input, output, format, tools, reference) = {
        let state = &app.ui.retouch_ai;
        (
            state.batch_input.trim().to_string(),
            state.batch_output.trim().to_string(),
            state.batch_format.clone(),
            state.selected_tools(),
            state.reference_path.trim().to_string(),
        )
    };
    if input.is_empty() || output.is_empty() {
        set_batch_status(&mut app.ui.retouch_ai, tl!(text::CHOOSE_FOLDERS).into(), true);
        return;
    }
    if tools.is_empty() {
        set_batch_status(&mut app.ui.retouch_ai, tl!(text::SELECT_A_TOOL).into(), true);
        return;
    }
    let mut step = json!({"tools": tools});
    if !reference.is_empty() {
        step["referencePath"] = json!(reference);
    }
    let params = json!({
        "steps": [["retouchAI.run", step]],
        "input": input,
        "output": output,
        "format": format,
    });
    app.ui.retouch_ai.batch_running = true;
    let result = app.run("file.automate.batch", params);
    app.ui.retouch_ai.batch_running = false;
    let state = &mut app.ui.retouch_ai;
    match result {
        Ok(value) => {
            let files = value.get("files").and_then(Value::as_array).map_or(0, Vec::len);
            let errors = value.get("errors").and_then(Value::as_array).map_or(0, Vec::len);
            let text = crate::i18n::fmt(tl!(text::BATCH_FINISHED), &[("files", &files.to_string()), ("errors", &errors.to_string())]);
            set_batch_status(state, text, errors > 0);
        }
        Err(error) => set_batch_status(state, crate::i18n::fmt(tl!(text::BATCH_FAILED), &[("error", &error)]), true),
    }
}

fn apply(app: &mut PhotocraftApp, action: Action) {
    match action {
        Action::Analyze => analyze(app),
        Action::Run => run_combo(app),
        Action::SetOpacity { layer, opacity, gesture } => {
            let params = json!({
                "layer": layer,
                "opacity": opacity / 100.0,
                "coalesce": format!("retouch-ai-opacity:{layer}:{gesture}"),
            });
            if let Err(error) = app.run("layer.setProps", params) {
                set_status(&mut app.ui.retouch_ai, crate::i18n::fmt(tl!(text::LAYER_UPDATE_FAILED), &[("error", &error)]), true);
            }
        }
        Action::SetVisible { layer, visible } => {
            if let Err(error) = app.run("layer.setProps", json!({"layer": layer, "visible": visible})) {
                set_status(&mut app.ui.retouch_ai, crate::i18n::fmt(tl!(text::LAYER_UPDATE_FAILED), &[("error", &error)]), true);
            }
        }
        Action::PickReference => {
            let initial_dir = crate::file_dialog::last_used_dir(&app.ui.recent_files);
            let extensions = ["jpg", "jpeg", "png", "tif", "tiff", "psd", "webp", "heic"].iter().map(|e| (*e).to_string()).collect();
            let asked = app.ask_file(FileDialogRequest::Open { multiple: false, initial_dir, extensions: Some(extensions) }, |app, answer| {
                let path = match answer {
                    FileDialogAnswer::Paths(paths) => paths.into_iter().next(),
                    FileDialogAnswer::Contents(name, _) => Some(name),
                    _ => None,
                };
                if let Some(path) = path {
                    let name = file_name(&path);
                    let state = &mut app.ui.retouch_ai;
                    state.reference_path = path;
                    set_status(state, crate::i18n::fmt(tl!(text::REFERENCE_SET), &[("name", &name)]), false);
                }
                Ok(Value::Null)
            });
            if let Err(error) = asked
                && error != crate::file_dialog::CANCELLED
            {
                set_status(&mut app.ui.retouch_ai, crate::i18n::fmt(tl!(text::CHOOSE_REFERENCE_FAILED), &[("error", &error)]), true);
            }
        }
        Action::PickBatchInput => {
            let _ = app.ask_folder(|app, path| {
                app.ui.retouch_ai.batch_input = path;
                Ok(Value::Null)
            });
        }
        Action::PickBatchOutput => {
            let _ = app.ask_folder(|app, path| {
                app.ui.retouch_ai.batch_output = path;
                Ok(Value::Null)
            });
        }
        Action::RunBatch => run_batch(app),
    }
}

fn group_header(ui: &mut egui::Ui, state: &mut RetouchAiState, key: &str, title: &str) -> bool {
    let t = Tokens::get(ui.ctx());
    let mut open = !state.collapsed.iter().any(|c| c == key);
    widgets::hairline(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let chevron = egui::Rect::from_center_size(egui::pos2(rect.left() + 7.0, rect.center().y), vec2(12.0, 12.0));
    icons::paint(ui, chevron, if open { "chevron-down" } else { "chevron-right" }, 11.0, t.text_dim);
    ui.painter().text(egui::pos2(rect.left() + 18.0, rect.center().y), Align2::LEFT_CENTER, title, theme::semibold(12.0), t.text);
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::CollapsingHeader, true, open, title));
    if response.clicked() {
        if open {
            state.collapsed.push(key.to_string());
        } else {
            state.collapsed.retain(|c| c != key);
        }
        open = !open;
    }
    if open {
        ui.add_space(3.0);
    }
    open
}

fn tool_row(ui: &mut egui::Ui, state: &mut RetouchAiState, id: &str) {
    let t = Tokens::get(ui.ctx());
    let hint = tool_hint(id);
    let Some(tool) = state.tools.iter_mut().find(|tool| tool.id == id) else { return };
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 19.0), Sense::click());
    let box_rect = egui::Rect::from_center_size(egui::pos2(rect.left() + 7.0, rect.center().y), vec2(13.0, 13.0));
    widgets::check_box(ui.painter(), box_rect, tool.enabled, response.hovered(), response.is_pointer_button_down_on(), &t);
    let label = tool_label(id);
    ui.painter().text(
        egui::pos2(rect.left() + 20.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        theme::medium(12.0),
        if tool.enabled { t.text } else { t.text_dim },
    );
    ui.painter().text(egui::pos2(rect.right() - 2.0, rect.center().y), Align2::RIGHT_CENTER, format!("{:.0}%", tool.amount), theme::mono(10.5), t.text_faint);
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, tool.enabled, label));
    if response.on_hover_text(hint).clicked() {
        tool.enabled = !tool.enabled;
    }
    let enabled = tool.enabled;
    ui.horizontal(|ui| {
        ui.add_space(20.0);
        ui.spacing_mut().slider_width = (ui.available_width() - 12.0).max(80.0);
        ui.add_enabled_ui(enabled, |ui| {
            ui.add(egui::Slider::new(&mut tool.amount, 0.0..=100.0).show_value(false).suffix("%")).on_hover_text(hint);
        });
    });
    ui.add_space(2.0);
}

fn presets(ui: &mut egui::Ui, state: &mut RetouchAiState) {
    ui.horizontal(|ui| {
        ui.label(tl!(text::PRESET));
        let mut choices: Vec<(String, String)> = Vec::with_capacity(BUILTINS.len() + state.presets.len());
        for preset in &BUILTINS {
            choices.push((format!("builtin:{}", preset.key), builtin_label(preset.key).to_string()));
        }
        for preset in &state.presets {
            choices.push((format!("user:{}", preset.name), preset.name.clone()));
        }
        let mut chosen: Option<String> = None;
        egui::ComboBox::from_id_salt("retouch-ai-preset")
            .selected_text(state.preset_label())
            .width(180.0)
            .icon(widgets::chevron_icon)
            .show_ui(ui, |ui| {
                if ui.selectable_label(state.last_preset == CUSTOM, tl!(text::CUSTOM)).clicked() {
                    chosen = Some(CUSTOM.into());
                }
                for (key, label) in &choices {
                    if ui.selectable_label(state.last_preset == *key, label).clicked() {
                        chosen = Some(key.clone());
                    }
                }
            });
        if let Some(key) = chosen {
            if let Some(builtin) = key.strip_prefix("builtin:") {
                state.apply_builtin(builtin);
            } else if let Some(name) = key.strip_prefix("user:") {
                state.apply_user_preset(name);
            } else {
                state.last_preset = CUSTOM.into();
            }
        }
    });
    ui.horizontal(|ui| {
        let width = (ui.available_width() - 116.0).max(90.0);
        ui.add_sized([width, 20.0], egui::TextEdit::singleline(&mut state.preset_name).hint_text(tl!(text::PRESET_NAME)));
        if ui.button(tl!(text::SAVE)).clicked() {
            let name = state.preset_name.clone();
            match state.save_preset(&name) {
                Ok(()) => set_status(state, crate::i18n::fmt(tl!(text::PRESET_SAVED), &[("name", &name)]), false),
                Err(error) => set_status(state, error, true),
            }
        }
        let can_delete = state.last_preset.starts_with("user:");
        if ui.add_enabled(can_delete, egui::Button::new(tl!(text::DELETE))).clicked()
            && let Some(name) = state.delete_selected_preset()
        {
            set_status(state, crate::i18n::fmt(tl!(text::PRESET_DELETED), &[("name", &name)]), false);
        }
    });
}

fn reference_row(ui: &mut egui::Ui, state: &mut RetouchAiState, actions: &mut Vec<Action>) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label(tl!(text::REFERENCE_IMAGE));
        if ui.button(tl!(text::CHOOSE_REFERENCE)).clicked() {
            actions.push(Action::PickReference);
        }
    });
    let (text, tip) = if state.reference_path.is_empty() {
        (tl!(text::NO_REFERENCE).to_string(), String::new())
    } else {
        (file_name(&state.reference_path), state.reference_path.clone())
    };
    let label = ui.add(egui::Label::new(egui::RichText::new(text).size(11.0).color(Tokens::get(ui.ctx()).text_dim)).truncate());
    if !tip.is_empty() {
        label.on_hover_text(tip);
    }
}

fn results(ui: &mut egui::Ui, state: &mut RetouchAiState, actions: &mut Vec<Action>, finger: u64) {
    if !group_header(ui, state, "results", tl!(text::RESULTS)) {
        return;
    }
    if state.result_layers.is_empty() {
        ui.label(egui::RichText::new(tl!(text::NO_RESULTS)).size(11.0).color(Tokens::get(ui.ctx()).text_faint));
        return;
    }
    for layer in &mut state.result_layers {
        ui.horizontal(|ui| {
            let tip = if layer.visible { tl!(text::HIDE_LAYER) } else { tl!(text::SHOW_LAYER) };
            if icons::button(ui, if layer.visible { "eye" } else { "eye-off" }, 20.0, layer.visible, tip).clicked() {
                layer.visible = !layer.visible;
                actions.push(Action::SetVisible { layer: layer.layer, visible: layer.visible });
            }
            let label = ui.add(egui::Label::new(egui::RichText::new(&layer.name).size(12.0)).truncate());
            if tool_label(&layer.tool).is_empty() {
                label.on_hover_text(&layer.name);
            } else {
                label.on_hover_text(format!("{} — {}", tool_label(&layer.tool), layer.name));
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(22.0);
            ui.spacing_mut().slider_width = (ui.available_width() - 44.0).max(80.0);
            let response = ui.add(egui::Slider::new(&mut layer.opacity, 0.0..=100.0).show_value(false).suffix("%"));
            if response.changed() {
                actions.push(Action::SetOpacity { layer: layer.layer, opacity: layer.opacity, gesture: finger });
            }
            ui.label(egui::RichText::new(format!("{:.0}%", layer.opacity)).font(theme::mono(10.5)).color(Tokens::get(ui.ctx()).text_faint));
        });
        ui.add_space(2.0);
    }
}

/// A batch format's English source label; [`format_label`] translates it.
fn format_source(id: &str) -> &'static str {
    match id {
        "jpg" => text::JPEG,
        "png" => text::PNG,
        "tiff" => text::TIFF,
        _ => text::SAME_AS_SOURCE,
    }
}

fn format_label(id: &str) -> &'static str {
    tl!(format_source(id))
}

fn batch(ui: &mut egui::Ui, state: &mut RetouchAiState, actions: &mut Vec<Action>) {
    if !group_header(ui, state, "batch", tl!(text::BATCH)) {
        return;
    }
    folder_field(ui, tl!(text::INPUT_FOLDER), &mut state.batch_input, Action::PickBatchInput, actions);
    folder_field(ui, tl!(text::OUTPUT_FOLDER), &mut state.batch_output, Action::PickBatchOutput, actions);
    ui.horizontal(|ui| {
        ui.label(tl!(text::FORMAT));
        let mut chosen = None;
        egui::ComboBox::from_id_salt("retouch-ai-batch-format")
            .selected_text(format_label(&state.batch_format))
            .width(150.0)
            .icon(widgets::chevron_icon)
            .show_ui(ui, |ui| {
                for id in ["same", "jpg", "png", "tiff"] {
                    if ui.selectable_label(state.batch_format == id, format_label(id)).clicked() {
                        chosen = Some(id.to_string());
                    }
                }
            });
        if let Some(id) = chosen {
            state.batch_format = id;
        }
    });
    let response = ui.add_enabled(!state.batch_running, egui::Button::new(tl!(text::RUN_BATCH)));
    if response.clicked() {
        actions.push(Action::RunBatch);
    }
    if !state.batch_status.is_empty() {
        let color = if state.batch_status_error { Tokens::get(ui.ctx()).danger } else { Tokens::get(ui.ctx()).text_dim };
        ui.add(egui::Label::new(egui::RichText::new(&state.batch_status).size(11.0).color(color)).wrap());
    }
}

fn folder_field(ui: &mut egui::Ui, label: &str, value: &mut String, pick: Action, actions: &mut Vec<Action>) {
    ui.label(egui::RichText::new(label).size(11.0).color(Tokens::get(ui.ctx()).text_dim));
    ui.horizontal(|ui| {
        let width = (ui.available_width() - 70.0).max(90.0);
        ui.add_sized([width, 20.0], egui::TextEdit::singleline(value).hint_text(tl!(text::FOLDER_PATH)));
        if ui.button(tl!(text::BROWSE)).clicked() {
            actions.push(pick);
        }
    });
}

/// Window › Retouch AI. Draws nothing when the menu closed it; never needs a document.
pub fn window(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !app.ui.panels.retouch_ai {
        return;
    }
    app.ui.retouch_ai.ensure_tools();
    sync_results_to_document(app);
    let has_document = app.session.active_index().is_some();
    let finger = slider_gesture(ctx);
    let mut open = true;
    let mut actions = Vec::new();
    let t = Tokens::get(ctx);
    let frame = egui::Frame::NONE
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.card_border))
        .corner_radius(CornerRadius::same(t.radius_lg as u8))
        .shadow(egui::Shadow { offset: [0, 10], blur: 30, spread: 0, color: t.shadow })
        .inner_margin(egui::Margin::same(10));
    let canvas = app.last_canvas_rect;
    let max_height = (ctx.content_rect().height() * 0.72).max(360.0);
    // A borderless auto-sized window defaults to 420 points tall, which cuts the tool list after
    // the first group. Reserve the fixed chrome (title, engine row, optional hint) plus the body.
    let default_height = (max_height + 100.0).min((ctx.content_rect().height() - 48.0).max(360.0));
    egui::Window::new(tl!(text::TITLE))
        .id(egui::Id::new("retouch-ai"))
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .frame(frame)
        .default_pos(egui::pos2((canvas.right() - WIDTH - 30.0).max(canvas.left() + 8.0), canvas.top() + 24.0))
        .default_height(default_height)
        .show(ctx, |ui| {
            ui.set_width(WIDTH);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tl!(text::TITLE)).font(theme::semibold(13.0)).color(t.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::button(ui, "x", 20.0, false, tl!(text::CLOSE)).clicked() {
                        open = false;
                    }
                });
            });
            ui.add_space(5.0);
            widgets::hairline(ui);
            ui.add_space(5.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tl!(text::ENGINE)).size(11.0).color(t.text_faint));
                let (rect, _) = ui.allocate_exact_size(vec2(84.0, 18.0), Sense::hover());
                ui.painter().rect_filled(rect, t.radius_sm, t.field);
                // Phase 1 is local only: no Cloud entry, no network.
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, tl!(text::LOCAL_CPU), theme::medium(11.0), t.text_dim);
            });
            ui.add_space(4.0);
            if !has_document {
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
                ui.painter().rect_filled(rect, t.radius_sm, t.field);
                ui.painter().text(egui::pos2(rect.left() + 8.0, rect.center().y), Align2::LEFT_CENTER, tl!(text::OPEN_IMAGE_TO_START), theme::medium(11.5), t.warning);
                // Painted text has no accessibility node of its own; name the banner for tests,
                // screen readers and the control channel.
                let hint = ui.interact(rect, ui.id().with("retouch-ai-no-document"), Sense::hover());
                hint.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, tl!(text::OPEN_IMAGE_TO_START)));
            }
            egui::ScrollArea::vertical().id_salt("retouch-ai-body").max_height(max_height).auto_shrink([false, false]).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let state = &mut app.ui.retouch_ai;
                presets(ui, state);
                ui.add_space(2.0);
                for (index, (key, _)) in TOOL_GROUPS.iter().enumerate() {
                    let title = group_title(index);
                    if group_header(ui, state, key, title) {
                        for spec in TOOLS.iter().filter(|spec| spec.group == index) {
                            tool_row(ui, state, spec.id);
                        }
                        if index == 2 {
                            reference_row(ui, state, &mut actions);
                        }
                    }
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(has_document, egui::Button::new(tl!(text::ANALYZE))).clicked() {
                        actions.push(Action::Analyze);
                    }
                    if ui.add_enabled(has_document, egui::Button::new(tl!(text::RUN_SELECTED))).clicked() {
                        actions.push(Action::Run);
                    }
                });
                if !state.status.is_empty() {
                    let color = if state.status_error { t.danger } else { t.text_dim };
                    ui.add(egui::Label::new(egui::RichText::new(&state.status).size(11.0).color(color)).wrap());
                }
                if has_document {
                    results(ui, state, &mut actions, finger);
                }
                batch(ui, state, &mut actions);
                ui.add_space(2.0);
            });
        });
    if !open {
        app.ui.panels.retouch_ai = false;
    }
    for action in actions {
        apply(app, action);
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    fn app() -> PhotocraftApp {
        PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default())
    }

    #[test]
    fn default_state_selects_the_expected_tools_and_round_trips_through_serde() {
        let state = RetouchAiState::default();
        assert_eq!(state.tools.len(), TOOLS.len());
        for id in ["heal", "dodgeBurn", "skinTone", "eyeBrilliance", "teeth"] {
            let tool = state.tool(id).unwrap_or_else(|| panic!("{id} missing"));
            assert!(tool.enabled, "{id} is on by default");
            assert!((60.0..=80.0).contains(&tool.amount), "{id} starts at a sensible amount: {}", tool.amount);
        }
        for id in ["mattifier", "skinMask", "eyeVessels", "cleanHair", "fabricVolumes", "dustClean", "backgroundClean", "colorMatch"] {
            assert!(!state.enabled(id), "{id} starts off");
        }
        let json = serde_json::to_string(&state).expect("serialize");
        let back: RetouchAiState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(state, back);
        // An older layout without the field gets the whole default state.
        let fresh: RetouchAiState = serde_json::from_str("{}").expect("missing fields default");
        assert_eq!(fresh, RetouchAiState::default());
    }

    #[test]
    fn builtin_presets_apply_their_selection_and_amounts() {
        let mut state = RetouchAiState::default();
        assert!(state.apply_builtin("beautyStudio"));
        assert_eq!(state.last_preset, "builtin:beautyStudio");
        assert_eq!(state.preset_label(), builtin_label("beautyStudio"));
        for (id, amount) in BUILTINS[2].tools {
            let tool = state.tool(id).unwrap_or_else(|| panic!("{id} missing"));
            assert!(tool.enabled, "beautyStudio turns {id} on");
            assert_eq!(tool.amount, *amount, "{id} amount");
        }
        assert!(!state.enabled("dustClean"), "beautyStudio leaves the remaining tools off");
        assert_eq!(state.tools.iter().filter(|tool| tool.enabled).count(), BUILTINS[2].tools.len());

        // A user preset stores the selection and amounts, survives serde, and can be deleted.
        state.save_preset("My look").expect("save");
        let saved = state.presets.iter().find(|preset| preset.name == "My look").expect("preset saved");
        assert_eq!(saved.tools, state.tools);
        let json = serde_json::to_string(&state).expect("serialize");
        let mut back: RetouchAiState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.presets, state.presets);
        assert_eq!(back.delete_selected_preset().as_deref(), Some("My look"));
        assert!(back.presets.is_empty());
        assert_eq!(back.last_preset, CUSTOM);
        assert!(!state.apply_builtin("nope"));
    }

    #[test]
    fn menu_command_toggles_the_panel() {
        let mut app = app();
        let ctx = egui::Context::default();
        assert!(!app.ui.panels.retouch_ai);
        crate::menus::invoke(&mut app, &ctx, "window.toggle.retouchAI", json!({})).expect("toggle");
        assert!(app.ui.panels.retouch_ai);
        crate::menus::invoke(&mut app, &ctx, "window.panel.retouchAI", json!({})).expect("panel alias");
        assert!(!app.ui.panels.retouch_ai);
        crate::menus::invoke(&mut app, &ctx, "window.panel.retouchAI", json!({})).expect("panel alias");
        assert!(app.ui.panels.retouch_ai);
    }

    #[test]
    fn menu_catalog_uses_the_toggle_command_and_the_alias_works_in_a_custom_workspace() {
        let entry = crate::menu_catalog::CATALOG
            .iter()
            .find(|(path, label, _, _)| *label == "Retouch AI" && path.contains(&"Window"))
            .expect("Window › Retouch AI is in the catalog");
        assert_eq!(entry.3, "window.toggle.retouchAI");

        // A saved custom dock layout makes panel_docking answer for `window.panel.*`, but the
        // Retouch AI panel is not dockable: the alias must reach the real toggle command.
        let mut app = app();
        app.run("window.panel.move", json!({"panel":"swatches","anchor":"layers"})).expect("custom layout");
        assert!(app.ui.docking.is_some());
        let ctx = egui::Context::default();
        assert!(!app.ui.panels.retouch_ai);
        crate::menus::invoke(&mut app, &ctx, "window.panel.retouchAI", json!({})).expect("panel alias");
        assert!(app.ui.panels.retouch_ai);
        let item = crate::menus::menu_items(&app)
            .into_iter()
            .find(|item| item.label == "Retouch AI" && item.path.iter().any(|p| p == "Window"))
            .expect("Window › Retouch AI is in the menu");
        assert_eq!(item.checked, Some(true), "the menu shows Retouch AI checked");
        crate::menus::invoke(&mut app, &ctx, "window.panel.retouchAI", json!({})).expect("panel alias");
        assert!(!app.ui.panels.retouch_ai);
    }

    #[test]
    fn panel_draws_without_a_document() {
        let mut app = app();
        app.ui.panels.retouch_ai = true;
        let mut h = Harness::builder().with_size(vec2(900.0, 800.0)).build_ui_state(
            |ui, app: &mut PhotocraftApp| {
                let ctx = ui.ctx().clone();
                if !ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                    return;
                }
                window(app, &ctx);
            },
            app,
        );
        PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
        h.run_steps(4);
        assert!(h.state().ui.panels.retouch_ai);
        assert!(h.query_all_by_label("Open an image to start").next().is_some(), "the no-document hint is shown");
        // The run buttons are disabled with no document: only Batch and Analyze/Presets remain.
        assert_eq!(h.state().ui.retouch_ai.tools.len(), TOOLS.len());
    }

    /// The panel's data keys skip the literal scanner (the other complete catalogs fall back to
    /// English), so this test is what guarantees the Vietnamese catalog covers every new string.
    #[test]
    fn vietnamese_covers_every_panel_key() {
        let vi = crate::i18n::Lang::from_code("vi").expect("vi is registered");
        let mut keys: Vec<&str> = text::ALL.to_vec();
        for spec in TOOLS {
            assert!(!tool_source(spec.id).is_empty(), "no source label for {}", spec.id);
            assert!(!tool_hint_source(spec.id).is_empty(), "no hint for {}", spec.id);
            keys.push(tool_source(spec.id));
            keys.push(tool_hint_source(spec.id));
        }
        for index in 0..TOOL_GROUPS.len() {
            assert!(!group_source(index).is_empty(), "no source label for group {index}");
            keys.push(group_source(index));
        }
        for preset in BUILTINS {
            assert!(!builtin_source(preset.key).is_empty(), "no label for {}", preset.key);
            keys.push(builtin_source(preset.key));
        }
        for id in ["same", "jpg", "png", "tiff"] {
            keys.push(format_source(id));
        }
        let missing: Vec<_> = keys.iter().copied().filter(|key| !crate::i18n::has(vi, key)).collect();
        assert!(missing.is_empty(), "vi: missing Retouch AI panel strings: {missing:#?}");
        // The data keys reach `tl!` correctly (not only the catalog): every label kind resolves.
        let (tool, group, preset, format) = crate::i18n::with_language(vi, || {
            (tool_label("heal"), group_title(0), builtin_label("idPhoto"), format_label("same"))
        });
        assert_eq!(tool, "Vá");
        assert_eq!(group, "Da & Mặt");
        assert_eq!(preset, "Ảnh thẻ");
        assert_eq!(format, "Giống ảnh gốc");
    }
}
