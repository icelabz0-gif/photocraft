//! Darkroom filters: Sepia, Chroma Denoise and Film Negative → Positive. Ported from Photoslop
//! (Apache-2.0, same author) onto normalized `f32` samples in any colour model.

use photocraft_geom::Rect;
use serde::{Deserialize, Serialize};

use crate::Ctx;
use crate::glitch::{fmt, luma, rgba_at};
use crate::image::Image;

/// What kind of negative Film Negative develops.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum NegativeMode {
    /// Decide from the mean chroma: an orange mask puts a colour negative far from neutral.
    #[default]
    Auto,
    Color,
    Mono,
}

/// Sepia tone: the colour moves `amount` % of the way from itself to the classic warm
/// monochrome of its luma (alpha untouched).
pub(crate) fn sepia(src: &Image, out: Rect, ctx: &Ctx, amount: f32) -> Vec<f32> {
    let n = src.ch;
    let mut res = src.crop(out);
    if out.is_empty() || n == 0 {
        return res;
    }
    let k = (amount / 100.0).clamp(0.0, 1.0);
    let f = fmt(ctx);
    for (i, px) in res.chunks_exact_mut(n).enumerate() {
        let x = out.x0 + (i % out.width() as usize) as i32;
        let y = out.y0 + (i / out.width() as usize) as i32;
        let c = rgba_at(src, &f, x, y);
        let l = luma(&c);
        let s = [(l * 1.07).min(1.0), l * 0.89, l * 0.62];
        let mut o = c;
        for ch in 0..3 {
            o[ch] = (c[ch] + (s[ch] - c[ch]) * k).clamp(0.0, 1.0);
        }
        photocraft_raster::from_rgba_into(&f, o, px);
    }
    res
}

/// Chroma Denoise reach in pixels for `strength` (1–100): three box passes of this radius.
pub(crate) fn denoise_radius(strength: f32) -> i32 {
    ((strength.clamp(1.0, 100.0) / 10.0).floor() as i32).max(1)
}

/// Chroma denoise: luma is preserved exactly; the two chroma differences (B−Y, R−Y) get three
/// box passes of `radius` (≈ Gaussian), the classic fast fix for colour speckle. Pixels outside
/// `src` repeat its edge.
pub(crate) fn chroma_denoise(src: &Image, out: Rect, ctx: &Ctx, strength: f32) -> Vec<f32> {
    let n = src.ch;
    let mut res = src.crop(out);
    if out.is_empty() || n == 0 || src.rect.is_empty() {
        return res;
    }
    let r = denoise_radius(strength);
    let f = fmt(ctx);
    // Chroma planes over the whole source window (the tile plus its halo).
    let sr = src.rect;
    let (w, h) = (sr.width() as usize, sr.height() as usize);
    let Some(len) = w.checked_mul(h) else { return res };
    let mut cb = vec![0.0f32; len];
    let mut cr = vec![0.0f32; len];
    for y in 0..h {
        for x in 0..w {
            let c = rgba_at(src, &f, sr.x0 + x as i32, sr.y0 + y as i32);
            let l = luma(&c);
            if let (Some(b), Some(rr)) = (cb.get_mut(y * w + x), cr.get_mut(y * w + x)) {
                *b = c[2] - l;
                *rr = c[0] - l;
            }
        }
    }
    let cb = box3(&cb, w, h, r as usize);
    let cr = box3(&cr, w, h, r as usize);
    for (i, px) in res.chunks_exact_mut(n).enumerate() {
        let x = out.x0 + (i % out.width() as usize) as i32;
        let y = out.y0 + (i / out.width() as usize) as i32;
        if !sr.contains(x, y) {
            continue;
        }
        let c = rgba_at(src, &f, x, y);
        let l = luma(&c);
        let k = (y - sr.y0) as usize * w + (x - sr.x0) as usize;
        let (b, rr) = (cb.get(k).copied().unwrap_or(0.0), cr.get(k).copied().unwrap_or(0.0));
        let red = (l + rr).clamp(0.0, 1.0);
        let blue = (l + b).clamp(0.0, 1.0);
        let green = ((l - 0.299 * red - 0.114 * blue) / 0.587).clamp(0.0, 1.0);
        photocraft_raster::from_rgba_into(&f, [red, green, blue, c[3]], px);
    }
    res
}

/// Three separable box passes of radius `r` over a `w × h` plane, edges clamped. Each output is a
/// direct sum over its window (never a running sum): the window is small (`r ≤ 10`) and a direct
/// sum adds the same values in the same order whatever the tile, so results don't depend on
/// where the tile's source window starts.
fn box3(p: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut cur = p.to_vec();
    if r == 0 || w == 0 || h == 0 {
        return cur;
    }
    let k = (2 * r + 1) as f32;
    let span = -(r as isize)..=(r as isize);
    let mut tmp = vec![0.0f32; cur.len()];
    for _ in 0..3 {
        for y in 0..h {
            let row = cur.get(y * w..(y + 1) * w).unwrap_or(&[]);
            let at = |x: isize| row.get(x.clamp(0, w as isize - 1) as usize).copied().unwrap_or(0.0);
            for x in 0..w {
                let sum: f32 = span.clone().map(|d| at(x as isize + d)).sum();
                if let Some(o) = tmp.get_mut(y * w + x) {
                    *o = sum / k;
                }
            }
        }
        let at = |x: usize, y: isize| tmp.get(y.clamp(0, h as isize - 1) as usize * w + x).copied().unwrap_or(0.0);
        for y in 0..h {
            for x in 0..w {
                let sum: f32 = span.clone().map(|d| at(x, y as isize + d)).sum();
                if let Some(o) = cur.get_mut(y * w + x) {
                    *o = sum / k;
                }
            }
        }
    }
    cur
}

/// Film Negative's development: per-channel scan bounds in reciprocal (transmittance) space.
///
/// Not an inversion. A colour negative carries an orange mask, so `1 − v` leaves a muddy cyan
/// cast; and film records transmittance, so the positive is a reciprocal, not a subtraction.
/// Normalising each channel independently between its own clipped extremes in `1/v` space does
/// both: the mask is a constant per channel and divides out, and the reciprocal gets film's
/// tonality right. A black-and-white negative has no mask and would only pick up a cast, so it
/// is developed from luma and written back neutral.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Negative {
    mono: bool,
    /// Densest kept scan value per channel (→ brightest positive) and the film base (→ black).
    lo: [f32; 3],
    hi: [f32; 3],
}

/// Mean per-pixel chroma (0–255) above which an `Auto` scan is a colour negative. An orange mask
/// lands far above it (typically 40–80) and a greyscale scan at essentially zero.
pub(crate) const MONO_CHROMA_MAX: f32 = 24.0;

impl Negative {
    /// Measures the bounds over the opaque pixels of `src` in `ctx.bounds`. `None` when there
    /// is nothing to develop.
    pub(crate) fn new(src: &Image, ctx: &Ctx, mode: NegativeMode, clip: f32) -> Option<Negative> {
        let b = ctx.bounds.intersect(&src.rect);
        if b.is_empty() {
            return None;
        }
        let f = fmt(ctx);
        let mut hist = [[0u64; 256]; 4];
        let mut chroma_sum = 0.0f64;
        let mut opaque = 0u64;
        let bin = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as usize;
        for y in b.y0..b.y1 {
            for x in b.x0..b.x1 {
                let c = rgba_at(src, &f, x, y);
                if c[3] <= 0.0 {
                    continue;
                }
                for (h, v) in hist.iter_mut().zip(c.iter()).take(3) {
                    if let Some(h) = h.get_mut(bin(*v)) {
                        *h += 1;
                    }
                }
                if let Some(h) = hist.get_mut(3).and_then(|h| h.get_mut(bin(luma(&c)))) {
                    *h += 1;
                }
                let top = c[0].max(c[1]).max(c[2]);
                let bottom = c[0].min(c[1]).min(c[2]);
                chroma_sum += f64::from((top - bottom) * 255.0);
                opaque += 1;
            }
        }
        if opaque == 0 {
            return None;
        }
        let mono = match mode {
            NegativeMode::Mono => true,
            NegativeMode::Color => false,
            NegativeMode::Auto => (chroma_sum / opaque as f64) <= f64::from(MONO_CHROMA_MAX),
        };
        let clip = if clip.is_finite() { clip.clamp(0.0, 49.0) } else { 0.5 };
        let bounds = |h: &[u64; 256]| clipped_bounds(h, opaque, clip);
        let (lo, hi) = if mono {
            let (l, h) = bounds(&hist[3]);
            ([l; 3], [h; 3])
        } else {
            let r = bounds(&hist[0]);
            let g = bounds(&hist[1]);
            let bl = bounds(&hist[2]);
            ([r.0, g.0, bl.0], [r.1, g.1, bl.1])
        };
        Some(Negative { mono, lo, hi })
    }

    /// One channel: scan value → developed positive, in transmittance space between the bounds.
    #[inline]
    fn channel(v: f32, lo: f32, hi: f32) -> f32 {
        let floor = 1.0 / 255.0;
        let recip = 1.0 / v.max(floor);
        let top = 1.0 / lo.max(floor);
        let bottom = 1.0 / hi.max(floor);
        let span = top - bottom;
        if span <= 0.0 {
            return 0.0;
        }
        ((recip - bottom) / span).clamp(0.0, 1.0)
    }

    /// Develops one straight RGBA pixel (alpha kept).
    pub(crate) fn develop(&self, c: [f32; 4]) -> [f32; 4] {
        if self.mono {
            let g = Self::channel(luma(&c), self.lo[0], self.hi[0]);
            [g, g, g, c[3]]
        } else {
            [Self::channel(c[0], self.lo[0], self.hi[0]), Self::channel(c[1], self.lo[1], self.hi[1]), Self::channel(c[2], self.lo[2], self.hi[2]), c[3]]
        }
    }
}

/// Lowest and highest scan value (0…1) left after discarding `clip` percent of the pixels from
/// each end: the film base and any dust speck or scanner flare that would otherwise define the
/// whole range on its own. A flat frame keeps the full range.
fn clipped_bounds(counts: &[u64; 256], total: u64, clip: f32) -> (f32, f32) {
    let drop = (total as f64 * f64::from(clip) / 100.0) as u64;
    let mut cum = 0u64;
    let mut lo = 255usize;
    let mut hi = 0usize;
    let mut lo_set = false;
    for (i, &c) in counts.iter().enumerate() {
        cum += c;
        if !lo_set && cum > drop {
            lo = i;
            lo_set = true;
        }
        if cum >= total.saturating_sub(drop) {
            hi = i;
            break;
        }
    }
    if hi <= lo {
        return (0.0, 1.0);
    }
    (lo as f32 / 255.0, hi as f32 / 255.0)
}
