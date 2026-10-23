//! The eight-band HSL Color Mixer shared by Camera Raw and Basic Color.
//!
//! The bands are the Color Mixer's, in display order: Red, Orange, Yellow, Green, Mint/Aqua, Blue,
//! Purple and Magenta, centred at [`HSL_BANDS`] degrees. A hue belongs to its two neighbouring
//! bands through hat weights, so a shift fades smoothly from one band into the next.
//!
//! This is the mixer Camera Raw has always applied after its presence stage; [`mix`] is that math
//! verbatim so both adjustment layers stay pixel-identical instead of drifting apart.

/// Hue band centres of the Color Mixer (degrees).
pub const HSL_BANDS: [f32; 8] = [0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 270.0, 300.0];

/// Rec. 709 luma of an RGB triple.
fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// Weights of the eight bands at hue `h` (degrees): hat functions between neighbouring centres.
pub fn band_weights(h: f32) -> [f32; 8] {
    let mut w = [0.0f32; 8];
    for k in 0..8 {
        let (c, prev, next) = (HSL_BANDS[k], HSL_BANDS[(k + 7) % 8], HSL_BANDS[(k + 1) % 8]);
        let diff = ((h - c + 540.0) % 360.0) - 180.0;
        let span = if diff >= 0.0 { (next - c).rem_euclid(360.0) } else { (c - prev).rem_euclid(360.0) };
        w[k] = (1.0 - diff.abs() / span.max(1e-3)).max(0.0);
    }
    w
}

/// Whether every band of the three mixer arrays is neutral (0).
pub fn is_neutral(hue: [f32; 8], sat: [f32; 8], lum: [f32; 8]) -> bool {
    hue.iter().chain(&sat).chain(&lum).all(|v| *v == 0.0)
}

/// HSV of an RGB triple: hue in degrees, saturation and value in 0..=1.
fn rgb_to_hsv(c: [f32; 3]) -> [f32; 3] {
    let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    let d = mx - mn;
    let h = if d <= 1e-9 {
        0.0
    } else if mx == c[0] {
        60.0 * ((c[1] - c[2]) / d).rem_euclid(6.0)
    } else if mx == c[1] {
        60.0 * ((c[2] - c[0]) / d + 2.0)
    } else {
        60.0 * ((c[0] - c[1]) / d + 4.0)
    };
    [h, if mx > 1e-9 { d / mx } else { 0.0 }, mx]
}

/// RGB of an HSV triple (hue in degrees).
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r + m, g + m, b + m]
}

/// The Color Mixer on one colour: hue shifts up to ±30° at ±100, saturation scales around the
/// colour's own saturation, and luminance is a luma-preserving brightness shift weighted by that
/// saturation (so neutrals never gain chroma).
///
/// `hue`, `sat` and `lum` are the per-band values in -100..=100. The caller must skip all-zero
/// arrays when it needs a bit-exact no-op; [`is_neutral`] answers that.
pub fn mix(c: [f32; 3], hue: [f32; 8], sat: [f32; 8], lum: [f32; 8]) -> [f32; 3] {
    if !c.iter().all(|v| v.is_finite()) {
        return c;
    }
    let [h, s, v] = rgb_to_hsv(c.map(|x| x.max(0.0)));
    if s <= 1e-4 {
        // Black, white and grey have no hue to shift and must not be tinted.
        return c;
    }
    let wts = band_weights(h);
    let (mut dh, mut ds, mut dl) = (0.0f32, 0.0f32, 0.0f32);
    for k in 0..8 {
        dh += wts[k] * hue[k];
        ds += wts[k] * sat[k];
        dl += wts[k] * lum[k];
    }
    let nh = h + dh / 100.0 * 30.0;
    let ns = (s * (1.0 + ds / 100.0)).clamp(0.0, 1.0);
    let mut out = hsv_to_rgb(nh, ns, v);
    // Keep the luminance, then apply the luminance shift weighted by saturation.
    let (l0, l1) = (luma(c), luma(out));
    if l1 > 1e-6 {
        out = out.map(|x| x * l0 / l1);
    }
    let k = 2f32.powf(dl / 100.0 * s);
    out.map(|x| x * k)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arr(v: [f32; 8]) -> ([f32; 8], [f32; 8], [f32; 8]) {
        (v, [0.0; 8], [0.0; 8])
    }

    #[test]
    fn band_centres_and_hat_weights() {
        for (k, centre) in HSL_BANDS.iter().enumerate() {
            let w = band_weights(*centre);
            assert!((w[k] - 1.0).abs() < 1e-6, "band {k} at its centre: {w:?}");
            assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-5, "weights partition 1 at {centre}: {w:?}");
        }
        // Halfway between red (0°) and orange (30°) both bands share the pixel equally.
        let w = band_weights(15.0);
        assert!((w[0] - 0.5).abs() < 1e-5 && (w[1] - 0.5).abs() < 1e-5, "{w:?}");
        // The red/magenta wrap works across 0°.
        let w = band_weights(330.0);
        assert!((w[0] - 0.5).abs() < 1e-5 && (w[7] - 0.5).abs() < 1e-5, "{w:?}");
    }

    #[test]
    fn zero_mixer_is_neutral_and_greys_never_tint() {
        assert!(is_neutral([0.0; 8], [0.0; 8], [0.0; 8]));
        assert!(!is_neutral([1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], [0.0; 8], [0.0; 8]));
        for v in [0.0, 0.25, 0.5, 0.9, 1.0] {
            let (hue, sat, lum) = ([100.0; 8], [100.0; 8], [100.0; 8]);
            assert_eq!(mix([v; 3], hue, sat, lum), [v; 3], "grey {v} must stay neutral");
        }
    }

    #[test]
    fn bands_move_their_own_hue_and_leave_others_alone() {
        // Pure red shifted by +100 in its band: hue goes towards orange (green rises).
        let (hue, sat, lum) = arr([100.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let red = mix([1.0, 0.0, 0.0], hue, sat, lum);
        assert!(red[1] > 0.05 && red[2] <= 1e-5, "{red:?}");
        // The blue band leaves red alone.
        let (hue, sat, lum) = arr([0.0, 0.0, 0.0, 0.0, 0.0, 100.0, 0.0, 0.0]);
        assert_eq!(mix([1.0, 0.0, 0.0], hue, sat, lum), [1.0, 0.0, 0.0]);
        // Saturation -100 flattens red; luminance is luma-preserving and weighted by saturation.
        let (hue, sat, lum) = ([0.0; 8], [-100.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], [0.0; 8]);
        let flat = mix([1.0, 0.0, 0.0], hue, sat, lum);
        assert!((flat[0] - flat[1]).abs() < 1e-5 && (flat[1] - flat[2]).abs() < 1e-5, "{flat:?}");
        let (hue, sat, lum) = ([0.0; 8], [0.0; 8], [100.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let bright = mix([0.6, 0.1, 0.1], hue, sat, lum);
        assert!(luma(bright) > luma([0.6, 0.1, 0.1]), "{bright:?}");
    }
}
