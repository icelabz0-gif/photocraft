//! Window › Color Presets: named colour/tone looks captured from a document's visible
//! adjustment layers and applied as a new pass-through group.
//!
//! A preset keeps whole-image colour work only: the adjustment kind, the source layer name, the
//! complete parameter set (`crate::adjust_params::to_params`), layer opacity and blend mode.
//! Masks, painted pixels, effects, groups and every other layer kind are not part of a look.
//! Applying a preset never edits existing layers: it inserts a new top-level group named
//! `Preset • <name>` whose children are fresh adjustment layers, in one undo step.
//!
//! Built-in looks are our own (clean-room), each a single Basic Color layer.

use photocraft_color::{BlendMode, ColorMode};
use photocraft_doc::{Adjustment, Layer, LayerContent};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Group, Named, always, bad, edit_groups, find, group_index, has_doc, req_str, str_param, unique_name};
use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

/// Built-in group holding the five starter looks.
pub const BUILTIN_GROUP: &str = "PhotoCraft";
/// Group a capture lands in when the caller names no group.
pub const USER_GROUP: &str = "My Presets";

/// A whole colour/tone look: adjustment layers in compositing order, bottom-to-top.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorPreset {
    pub name: String,
    pub layers: Vec<ColorPresetLayer>,
}

/// One adjustment layer of a preset. Only whole-image colour data is kept.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorPresetLayer {
    /// Adjustment kind id, as `crate::commands::adjustment_kind` writes it.
    pub kind: String,
    /// Source layer name; empty means the kind's display label when applied.
    pub name: String,
    /// Complete `crate::adjust_params::to_params` payload.
    pub params: Value,
    /// Layer opacity, clamped to `0..=1` when applied.
    pub opacity: f32,
    /// `BlendMode` serde name (`"Normal"`, `"Multiply"`, …).
    pub blend: String,
}

impl Default for ColorPresetLayer {
    fn default() -> Self {
        ColorPresetLayer { kind: String::new(), name: String::new(), params: json!({}), opacity: 1.0, blend: "Normal".into() }
    }
}

impl Named for ColorPreset {
    fn name(&self) -> &str {
        &self.name
    }

    fn set_name(&mut self, n: String) {
        self.name = n;
    }
}

impl ColorPreset {
    /// The list/result JSON shape: `{name, layers:[{kind,name,params,opacity,blend}]}`.
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "layers": self.layers.iter().map(ColorPresetLayer::to_json).collect::<Vec<_>>(),
        })
    }
}

impl ColorPresetLayer {
    pub fn to_json(&self) -> Value {
        json!({
            "kind": self.kind,
            "name": self.name,
            "params": self.params,
            "opacity": self.opacity,
            "blend": self.blend,
        })
    }
}

/// The five starter looks, all our own. Each is one Basic Color layer at full opacity, Normal.
pub fn builtin() -> Vec<Group<ColorPreset>> {
    let look = |name: &str, params: Value| ColorPreset {
        name: name.into(),
        layers: vec![ColorPresetLayer { kind: "basicColor".into(), name: "Basic Color".into(), params, opacity: 1.0, blend: "Normal".into() }],
    };
    vec![Group::new(
        BUILTIN_GROUP,
        vec![
            look(
                "Warm Portrait",
                json!({
                    "temperature": 25.0, "tint": -5.0, "exposure": 0.15, "contrast": 10.0, "highlights": -15.0,
                    "shadows": 15.0, "whites": 0.0, "blacks": 0.0, "vibrance": 15.0, "saturation": 0.0,
                }),
            ),
            look(
                "Cool Soft",
                json!({
                    "temperature": -20.0, "tint": 5.0, "exposure": 0.05, "contrast": -5.0, "highlights": -10.0,
                    "shadows": 10.0, "whites": 0.0, "blacks": 5.0, "vibrance": 0.0, "saturation": -5.0,
                }),
            ),
            look(
                "Punchy",
                json!({
                    "temperature": 5.0, "tint": 0.0, "exposure": 0.0, "contrast": 25.0, "highlights": -10.0,
                    "shadows": 5.0, "whites": 10.0, "blacks": -10.0, "vibrance": 25.0, "saturation": 5.0,
                }),
            ),
            look(
                "Faded Film",
                json!({
                    "temperature": 10.0, "tint": 3.0, "exposure": 0.0, "contrast": -15.0, "highlights": -20.0,
                    "shadows": 20.0, "whites": -5.0, "blacks": 15.0, "vibrance": -5.0, "saturation": -15.0,
                }),
            ),
            look(
                "Black & White",
                json!({
                    "temperature": 0.0, "tint": 0.0, "exposure": 0.0, "contrast": 15.0, "highlights": 0.0,
                    "shadows": 0.0, "whites": 0.0, "blacks": -5.0, "vibrance": 0.0, "saturation": -100.0,
                }),
            ),
        ],
    )]
}

/// Read the saved set leniently: a damaged group or preset is dropped without discarding the
/// rest. `None` when the value is not a list at all, so a malformed set keeps the defaults.
pub(crate) fn load(value: Value) -> Option<Vec<Group<ColorPreset>>> {
    let groups = value.as_array()?;
    let mut out = Vec::with_capacity(groups.len());
    for group in groups {
        let Some(name) = group.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()) else { continue };
        let Some(items) = group.get("items").and_then(Value::as_array) else { continue };
        let mut presets = Vec::with_capacity(items.len());
        for item in items {
            let Ok(mut preset) = serde_json::from_value::<ColorPreset>(item.clone()) else { continue };
            let name = preset.name.trim().to_string();
            if name.is_empty() {
                continue;
            }
            preset.name = name;
            presets.push(preset);
        }
        out.push(Group::new(name, presets));
    }
    Some(out)
}

/// A blend mode as its serde name, the form `ColorPresetLayer.blend` stores.
fn blend_name(blend: BlendMode) -> String {
    serde_json::to_value(blend).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "Normal".into())
}

/// The next free automatic name: `Look 1`, `Look 2`, ….
fn next_look_name(groups: &[Group<ColorPreset>]) -> String {
    (1..).map(|i| format!("Look {i}")).find(|n| find(groups, n, None).is_none()).unwrap_or_else(|| "Look 1".into())
}

/// Validate every layer before the document changes; `Err` leaves no partial stack.
fn ready_layers(preset: &ColorPreset, mode: ColorMode, cmd: &str) -> Result<Vec<ReadyLayer>> {
    if preset.layers.is_empty() {
        return Err(bad(cmd, format!("preset \"{}\" has no layers", preset.name)));
    }
    preset
        .layers
        .iter()
        .enumerate()
        .map(|(i, layer)| {
            let n = i + 1;
            let kind = layer.kind.trim();
            if kind.is_empty() {
                return Err(bad(cmd, format!("layer {n} has no adjustment kind")));
            }
            let adjustment = crate::adjust_params::from_params(kind, &layer.params, None, mode).map_err(|e| bad(cmd, format!("layer {n} ({kind}): {e}")))?;
            if !layer.opacity.is_finite() {
                return Err(bad(cmd, format!("layer {n} ({kind}): opacity is not a number")));
            }
            let blend = crate::commands::blend_from_str(layer.blend.trim())
                .filter(|b| *b != BlendMode::PassThrough)
                .ok_or_else(|| bad(cmd, format!("layer {n} ({kind}): unknown blend mode \"{}\"", layer.blend)))?;
            let name = if layer.name.trim().is_empty() { adjustment.label().to_string() } else { layer.name.trim().to_string() };
            Ok(ReadyLayer { name, adjustment, opacity: layer.opacity.clamp(0.0, 1.0), blend })
        })
        .collect()
}

struct ReadyLayer {
    name: String,
    adjustment: Adjustment,
    opacity: f32,
    blend: BlendMode,
}

fn list(s: &mut Session, _: &Value) -> Result<Value> {
    let groups: Vec<Value> =
        s.presets.color_presets.iter().map(|g| json!({"name": g.name, "presets": g.items.iter().map(ColorPreset::to_json).collect::<Vec<_>>()})).collect();
    Ok(json!({"groups": groups}))
}

fn new_preset(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "color.presets.new";
    let mut layers = Vec::new();
    {
        let d = s.active().ok_or(EngineError::NoDocument)?;
        for (path, _, layer) in d.doc.walk() {
            // A hidden layer, or anything inside a hidden group, is not part of the look.
            if !d.doc.shown_at(&path) {
                continue;
            }
            let LayerContent::Adjustment(adjustment) = &layer.content else { continue };
            // Unsupported adjustments only exist as imported data; there is no parameter set to
            // recreate them from, so they cannot be saved into a preset.
            if matches!(adjustment, Adjustment::Unsupported { .. }) {
                continue;
            }
            layers.push(ColorPresetLayer {
                kind: crate::commands::adjustment_kind(adjustment).to_string(),
                name: layer.name.clone(),
                params: crate::adjust_params::to_params(adjustment),
                opacity: layer.opacity.clamp(0.0, 1.0),
                blend: blend_name(layer.blend),
            });
        }
    }
    if layers.is_empty() {
        return Err(bad(CMD, "no visible adjustment layers to save"));
    }
    let gi = group_index(&mut s.presets.color_presets, Some(str_param(p, "group").unwrap_or(USER_GROUP)));
    let name = match str_param(p, "name") {
        // Uniqueness is per group, as in the contract: the same name may exist in another group
        // and `apply`'s optional `group` narrows the lookup.
        Some(name) => unique_name(std::slice::from_ref(&s.presets.color_presets[gi]), name),
        None => next_look_name(&s.presets.color_presets),
    };
    s.presets.color_presets[gi].items.push(ColorPreset { name: name.clone(), layers });
    let group = s.presets.color_presets[gi].name.clone();
    let count = s.presets.color_presets[gi].items.last().map_or(0, |p| p.layers.len());
    s.presets_changed();
    Ok(json!({"name": name, "group": group, "layers": count}))
}

/// The preset named by `"preset"`: a library name (optionally narrowed by `"group"`) or an
/// inline `{name, layers}` object for automation and tests.
fn preset_param(s: &Session, p: &Value, cmd: &str) -> Result<ColorPreset> {
    match p.get("preset") {
        Some(Value::String(name)) => {
            let name = name.trim();
            if name.is_empty() {
                return Err(bad(cmd, "missing `preset`"));
            }
            let (gi, ii) = find(&s.presets.color_presets, name, str_param(p, "group"))
                .ok_or_else(|| bad(cmd, format!("no color preset \"{name}\" (see color.presets.list)")))?;
            Ok(s.presets.color_presets[gi].items[ii].clone())
        }
        Some(v @ Value::Object(_)) => {
            let mut preset: ColorPreset = serde_json::from_value(v.clone()).map_err(|e| bad(cmd, format!("bad inline preset: {e}")))?;
            let name = preset.name.trim().to_string();
            if name.is_empty() {
                return Err(bad(cmd, "an inline preset needs a name"));
            }
            preset.name = name;
            Ok(preset)
        }
        Some(_) => Err(bad(cmd, "`preset` is a name or {name,layers}")),
        None => Err(bad(cmd, "missing `preset`")),
    }
}

fn apply(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "color.presets.apply";
    let preset = preset_param(s, p, CMD)?;
    let mode = s.active().ok_or(EngineError::NoDocument)?.doc.mode;
    let ready = ready_layers(&preset, mode, CMD)?;
    let n = ready.len();
    let group_name = format!("Preset \u{2022} {}", preset.name);
    let label = format!("Apply Preset \u{201c}{}\u{201d}", preset.name);
    let (gid, ids) = s.edit(&label, |doc, active| {
        let mut children = Vec::with_capacity(n);
        let mut ids = Vec::with_capacity(n);
        for layer in ready {
            let mut l = Layer::new(layer.name, LayerContent::Adjustment(layer.adjustment));
            l.opacity = layer.opacity;
            l.blend = layer.blend;
            ids.push(l.id);
            children.push(l);
        }
        // `Layer::group` uses Pass Through, so the adjustment layers reach the composite below.
        let gid = doc.insert_above(None, Layer::group(group_name.clone(), children));
        *active = Some(gid);
        Ok((gid, ids))
    })?;
    crate::layer_multi_cmds::reselect(s, vec![gid], Some(gid));
    Ok(json!({
        "name": preset.name,
        "group": gid.0,
        "layers": ids.iter().map(|id| id.0).collect::<Vec<_>>(),
    }))
}

fn edit(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "color.presets.edit";
    let action = req_str(p, "action", CMD)?.to_string();
    let r = edit_groups(&mut s.presets.color_presets, &action, p, CMD)?;
    s.presets_changed();
    Ok(r)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "color.presets.list",
            label: "Color Presets",
            menu: &[],
            shortcut: None,
            params: "{} → {groups:[{name,presets:[{name,layers:[{kind,name,params,opacity,blend}]}]}]}",
            enabled: always,
            run: list,
            journal: false,
        },
        CommandSpec {
            id: "color.presets.new",
            label: "New Color Preset…",
            menu: &[],
            shortcut: None,
            params: r##"{"name":str? (default a unique "Look 1", "Look 2"…),"group":name?="My Presets" (created when missing)} → {name,group,layers} (captures every visible adjustment layer, bottom-to-top, with opacity and blend; no masks, effects or groups)"##,
            enabled: has_doc,
            run: new_preset,
            journal: true,
        },
        CommandSpec {
            id: "color.presets.apply",
            label: "Apply Color Preset",
            menu: &[],
            shortcut: None,
            params: r##"{"preset":name|{name,layers:[{kind,name,params,opacity,blend}]},"group":name? (narrows a library lookup)} → {name,group,layers} (one new top-level Pass Through group at the top, one undo step; existing layers are never modified)"##,
            enabled: has_doc,
            run: apply,
            journal: true,
        },
        CommandSpec {
            id: "color.presets.edit",
            label: "Edit Color Presets",
            menu: &[],
            shortcut: None,
            params: super::GROUP_EDIT_PARAMS,
            enabled: always,
            run: edit,
            journal: true,
        },
    ]
}
