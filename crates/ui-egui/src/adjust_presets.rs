//! Preset menus for Levels, Exposure, Black & White and Channel Mixer (#9263, #9265, #9268,
//! #9270): starting settings named for the looks they give, as the Preset menu at the top of
//! these adjustments in Photoshop.
//!
//! The settings are PhotoCraft's own. Each was designed for its look: Levels presets lift, lower or
//! stretch the tones; Exposure presets are whole stops; the Black & White "filter" presets follow
//! what a coloured lens filter does on black-and-white film (it lightens its own colour and darkens
//! its complement); the Channel Mixer presets mix the channels a filter would let through. None is
//! taken or measured from another application's preset files.

use photocraft_doc::ColorMode;
use photocraft_engine::adjust_params::{from_params, to_params};
use serde_json::{Value, json};

/// A preset: its id (the menu value), label and the adjustment params it sets (the rest default).
pub struct AdjustPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub params: fn() -> Value,
}

macro_rules! preset {
    ($id:literal, $label:literal, $params:tt) => {
        AdjustPreset { id: $id, label: $label, params: || json!($params) }
    };
}

const LEVELS: &[AdjustPreset] = &[
    preset!("default", "Default", {}),
    // The whole image darker: the white output comes down.
    preset!("darker", "Darker", {"outWhite": 220}),
    // Black and white input points pulled in, a step further each time.
    preset!("increaseContrast1", "Increase Contrast 1", {"inBlack": 12, "inWhite": 243}),
    preset!("increaseContrast2", "Increase Contrast 2", {"inBlack": 24, "inWhite": 231}),
    preset!("increaseContrast3", "Increase Contrast 3", {"inBlack": 36, "inWhite": 219}),
    // Shadows lifted off black and opened up a little; highlights left where they are.
    preset!("lightenShadows", "Lighten Shadows", {"outBlack": 20, "gamma": 1.1}),
    // The whole image lighter: the white input point comes in.
    preset!("lighter", "Lighter", {"inWhite": 220}),
    // The midtones alone, up or down; black and white stay put.
    preset!("midtonesBrighter", "Midtones Brighter", {"gamma": 1.3}),
    preset!("midtonesDarker", "Midtones Darker", {"gamma": 0.77}),
];

const EXPOSURE: &[AdjustPreset] = &[
    preset!("default", "Default", {}),
    preset!("minus1", "Minus 1.0", {"exposure": -1.0}),
    preset!("minus2", "Minus 2.0", {"exposure": -2.0}),
    preset!("plus1", "Plus 1.0", {"exposure": 1.0}),
    preset!("plus2", "Plus 2.0", {"exposure": 2.0}),
];

// Black & White weights per colour range: reds, yellows, greens, cyans, blues, magentas.
const BLACK_WHITE: &[AdjustPreset] = &[
    preset!("default", "Default", {}),
    // A blue filter: blues and cyans light, reds and yellows dark.
    preset!("blueFilter", "Blue Filter", {"reds": 0, "yellows": 20, "greens": 40, "cyans": 100, "blues": 110, "magentas": 80}),
    // Every colour a little darker.
    preset!("darker", "Darker", {"reds": 20, "yellows": 40, "greens": 20, "cyans": 40, "blues": 0, "magentas": 60}),
    // A green filter: greens light (foliage), reds and magentas dark.
    preset!("greenFilter", "Green Filter", {"reds": 10, "yellows": 80, "greens": 110, "cyans": 60, "blues": 20, "magentas": 0}),
    // The blue filter pushed further: deep reds, glowing blues.
    preset!("highContrastBlueFilter", "High Contrast Blue Filter", {"reds": -40, "yellows": 0, "greens": 40, "cyans": 140, "blues": 160, "magentas": 100}),
    // The red filter pushed further: near-white reds, near-black blue skies.
    preset!("highContrastRedFilter", "High Contrast Red Filter", {"reds": 140, "yellows": 120, "greens": 0, "cyans": -40, "blues": -60, "magentas": 80}),
    // Infrared film: foliage greens and yellows glow, skies go dark.
    preset!("infrared", "Infrared", {"reds": -20, "yellows": 190, "greens": 200, "cyans": -30, "blues": -60, "magentas": 40}),
    // Every colour a little lighter.
    preset!("lighter", "Lighter", {"reds": 60, "yellows": 80, "greens": 60, "cyans": 80, "blues": 40, "magentas": 100}),
    // Colours as dark as they go.
    preset!("maximumBlack", "Maximum Black", {"reds": 0, "yellows": 0, "greens": 0, "cyans": 0, "blues": 0, "magentas": 0}),
    // Colours as light as they go.
    preset!("maximumWhite", "Maximum White", {"reds": 150, "yellows": 150, "greens": 150, "cyans": 150, "blues": 150, "magentas": 150}),
    // A neutral filter: every colour weighed the same.
    preset!("neutralDensity", "Neutral Density", {"reds": 50, "yellows": 50, "greens": 50, "cyans": 50, "blues": 50, "magentas": 50}),
    // A red filter: reds and yellows light, blues and cyans dark (dramatic skies).
    preset!("redFilter", "Red Filter", {"reds": 110, "yellows": 100, "greens": 0, "cyans": -20, "blues": -30, "magentas": 70}),
    // A yellow filter: a gentle darkening of blue skies.
    preset!("yellowFilter", "Yellow Filter", {"reds": 70, "yellows": 100, "greens": 50, "cyans": 30, "blues": 0, "magentas": 50}),
];

// Channel Mixer monochrome mixes `[red %, green %, blue %, constant %]`: what a filter lets through.
const CHANNEL_MIXER: &[AdjustPreset] = &[
    preset!("default", "Default", {}),
    preset!("bwInfrared", "Black & White Infrared (RGB)", {"monochrome": true, "gray": [-60, 190, -30, 0]}),
    preset!("bwBlueFilter", "Black & White with Blue Filter (RGB)", {"monochrome": true, "gray": [0, 0, 100, 0]}),
    preset!("bwGreenFilter", "Black & White with Green Filter (RGB)", {"monochrome": true, "gray": [0, 100, 0, 0]}),
    preset!("bwOrangeFilter", "Black & White with Orange Filter (RGB)", {"monochrome": true, "gray": [50, 50, 0, 0]}),
    preset!("bwRedFilter", "Black & White with Red Filter (RGB)", {"monochrome": true, "gray": [100, 0, 0, 0]}),
    preset!("bwYellowFilter", "Black & White with Yellow Filter (RGB)", {"monochrome": true, "gray": [34, 66, 0, 0]}),
];

/// The presets of adjustment `kind` for a document (`gray`: Grayscale, where the colour presets
/// don't apply); empty for a kind without a Preset menu.
pub fn presets(kind: &str, gray: bool) -> &'static [AdjustPreset] {
    match kind {
        "levels" => LEVELS,
        "exposure" => EXPOSURE,
        "blackWhite" if !gray => BLACK_WHITE,
        "channelMixer" if !gray => CHANNEL_MIXER,
        _ => &[],
    }
}

fn mode(gray: bool) -> ColorMode {
    if gray { ColorMode::Grayscale } else { ColorMode::Rgb }
}

/// `params` as the engine reads them: every key, defaults filled in.
fn normal(kind: &str, params: &Value, gray: bool) -> Option<Value> {
    from_params(kind, params, None, mode(gray)).ok().map(|a| to_params(&a))
}

/// The settings `preset` gives, with every other key at its default.
pub fn settings(kind: &str, preset: &AdjustPreset, gray: bool) -> Option<Value> {
    normal(kind, &(preset.params)(), gray)
}

/// The preset the values `v` are (Default for untouched values), or `None` (Custom).
pub fn matching(kind: &str, v: &Value, gray: bool) -> Option<&'static AdjustPreset> {
    let now = normal(kind, v, gray)?;
    presets(kind, gray).iter().find(|p| settings(kind, p, gray).as_ref() == Some(&now))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every preset is valid, distinct and recognised once applied; an edit makes it Custom.
    #[test]
    fn presets_apply_and_are_recognised() {
        for kind in ["levels", "exposure", "blackWhite", "channelMixer"] {
            let all = presets(kind, false);
            assert!(all.len() >= 5 && all[0].id == "default", "{kind}");
            for p in all {
                let s = settings(kind, p, false).unwrap_or_else(|| panic!("{kind} {}: valid params", p.id));
                assert_eq!(matching(kind, &s, false).map(|m| m.id), Some(p.id), "{kind} {}", p.id);
            }
            let distinct: std::collections::HashSet<String> = all.iter().map(|p| settings(kind, p, false).unwrap().to_string()).collect();
            assert_eq!(distinct.len(), all.len(), "{kind}: no two presets are the same");
            assert_eq!(matching(kind, &json!({}), false).map(|p| p.id), Some("default"), "{kind}: untouched is Default");
        }
        let mut v = settings("levels", &LEVELS[3], false).unwrap();
        v["gamma"] = json!(1.37);
        assert!(matching("levels", &v, false).is_none(), "an edit is Custom");
        assert!(presets("blackWhite", true).is_empty() && presets("channelMixer", true).is_empty(), "colour presets need colour");
        assert_eq!(presets("levels", true).len(), LEVELS.len());
    }

    /// The looks hold: contrast steps widen, filters lighten their own colour and darken the
    /// complement, exposure presets are whole stops.
    #[test]
    fn presets_give_their_looks() {
        let lv = |id: &str| settings("levels", LEVELS.iter().find(|p| p.id == id).unwrap(), false).unwrap();
        let span = |v: &Value| v["inWhite"].as_f64().unwrap() - v["inBlack"].as_f64().unwrap();
        assert!(span(&lv("increaseContrast1")) > span(&lv("increaseContrast2")) && span(&lv("increaseContrast2")) > span(&lv("increaseContrast3")));
        assert!(lv("midtonesBrighter")["gamma"].as_f64().unwrap() > 1.0 && lv("midtonesDarker")["gamma"].as_f64().unwrap() < 1.0);
        let bw = |id: &str| settings("blackWhite", BLACK_WHITE.iter().find(|p| p.id == id).unwrap(), false).unwrap();
        let w = |v: &Value, k: &str| v[k].as_f64().unwrap();
        assert!(w(&bw("redFilter"), "reds") > w(&bw("redFilter"), "cyans"), "a red filter lightens reds over cyans");
        assert!(w(&bw("blueFilter"), "blues") > w(&bw("blueFilter"), "yellows"), "a blue filter lightens blues over yellows");
        assert!(
            w(&bw("highContrastRedFilter"), "reds") - w(&bw("highContrastRedFilter"), "blues") > w(&bw("redFilter"), "reds") - w(&bw("redFilter"), "blues")
        );
        let ex = |id: &str| settings("exposure", EXPOSURE.iter().find(|p| p.id == id).unwrap(), false).unwrap()["exposure"].as_f64().unwrap();
        assert_eq!([ex("minus2"), ex("minus1"), ex("plus1"), ex("plus2")], [-2.0, -1.0, 1.0, 2.0]);
        for p in &CHANNEL_MIXER[1..] {
            let s = settings("channelMixer", p, false).unwrap();
            assert_eq!(s["monochrome"], json!(true), "{}", p.id);
        }
    }
}
