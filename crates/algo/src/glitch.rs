//! Glitch and retro looks: Pixel Sort, Datamosh + Chromatic Aberration, Retro Console and Beam
//! Dither. Ported from Photoslop (Apache-2.0, same author), re-derived here on normalized `f32`
//! samples so they run at every depth and in every colour model.
//!
//! The four read and write straight RGBA through [`photocraft_raster::to_rgba`] /
//! [`photocraft_raster::from_rgba_into`]. Two of them do work that is inherently whole-image
//! (sorting runs that span the bounds; serpentine error diffusion, whose every pixel depends on the
//! one before it), so they are *global* filters ([`crate::Halo::Bounds`]) and build a [`Prepared`]
//! once per run from the shared bounds image; tiles then only gather from it. Datamosh is global
//! too but its per-tile work is cheap (a grid of motion vectors), and Retro Console is a
//! neighbourhood filter like Mosaic.

use photocraft_color::{PixelFormat, SampleType};
use photocraft_geom::Rect;
use photocraft_raster::{from_rgba_into, to_rgba};
use serde::{Deserialize, Serialize};

use crate::image::Image;
use crate::noise::hash01;
use crate::{Ctx, FilterParams, par};

/// Dither / render algorithm of Beam Dither.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum DitherAlgorithm {
    /// CRT beam modulation: a raster of horizontal beams deflected and widened by the picture.
    #[default]
    Beam,
    FloydSteinberg,
    Atkinson,
    Jarvis,
    Stucki,
    Sierra,
    Burkes,
    Bayer2,
    Bayer4,
    Bayer8,
    /// Plain posterisation, no dithering.
    Threshold,
}

/// How Beam Dither inks the rendered tone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum BeamMode {
    /// One ink (`highlights`) on `background`.
    #[default]
    Mono,
    /// Tri-tone: lit pixels take the shadows / midtones / highlights ink by the *original* luma.
    Tonal,
    /// Each RGB channel dithered on its own (the indexed-palette look).
    Color,
}

/// Beam Dither's parameters (Photoslop's dialog units).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeamDitherSpec {
    pub algorithm: DitherAlgorithm,
    pub mode: BeamMode,
    /// Cell size in pixels (1–32): the picture is block-averaged down by it and rendered coarse.
    pub scale: u32,
    /// Tone levels (2–8).
    pub levels: u32,
    /// −100…100.
    pub brightness: f32,
    /// −100…100.
    pub contrast: f32,
    /// Blur radius px (0–20), applied before sharpening.
    pub blur: f32,
    /// Unsharp strength % (0–300) and radius px (1–20).
    pub sharpen: f32,
    pub sharpen_radius: f32,
    /// Positive adds seeded grain, negative blurs (−100…100).
    pub noise: f32,
    /// Beam pitch px (2–64) and displacement (0–8).
    pub beam_pitch: u32,
    pub beam_amplitude: f32,
    /// Inks, straight sRGB RGBA.
    pub highlights: [f32; 4],
    pub midtones: [f32; 4],
    pub shadows: [f32; 4],
    pub background: [f32; 4],
}

impl Default for BeamDitherSpec {
    fn default() -> Self {
        BeamDitherSpec {
            algorithm: DitherAlgorithm::Beam,
            mode: BeamMode::Mono,
            scale: 3,
            levels: 2,
            brightness: 0.0,
            contrast: 0.0,
            blur: 0.0,
            sharpen: 0.0,
            sharpen_radius: 2.0,
            noise: 0.0,
            beam_pitch: 6,
            beam_amplitude: 1.5,
            highlights: [1.0, 1.0, 1.0, 1.0],
            midtones: [176.0 / 255.0, 176.0 / 255.0, 176.0 / 255.0, 1.0],
            shadows: [90.0 / 255.0, 90.0 / 255.0, 90.0 / 255.0, 1.0],
            background: [0.0, 0.0, 0.0, 1.0],
        }
    }
}

pub(crate) fn fmt(ctx: &Ctx) -> PixelFormat {
    PixelFormat::new(ctx.mode, SampleType::F32, ctx.alpha)
}

/// Rec. 601 luma of straight RGB.
#[inline]
pub(crate) fn luma(rgb: &[f32; 4]) -> f32 {
    0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2]
}

/// Straight RGBA of the pixel at `(x, y)` (zero outside the image).
#[inline]
pub(crate) fn rgba_at(src: &Image, f: &PixelFormat, x: i32, y: i32) -> [f32; 4] {
    if src.rect.contains(x, y) { to_rgba(f, src.px(x, y)) } else { [0.0; 4] }
}

/// Copies the source pixels of `out`; the identity result every kernel starts from.
fn identity(src: &Image, out: Rect) -> Vec<f32> {
    src.crop(out)
}

/// Writes `rgba` into every pixel of `res` (the samples of `out`) at `(x, y)`.
#[inline]
fn put(res: &mut [f32], out: Rect, n: usize, f: &PixelFormat, x: i32, y: i32, rgba: [f32; 4]) {
    let i = ((y - out.y0) as usize * out.width() as usize + (x - out.x0) as usize) * n;
    if let Some(px) = res.get_mut(i..i + n) {
        from_rgba_into(f, rgba, px);
    }
}

// ---------------------------------------------------------------------------------------------
// Pixel Sort
// ---------------------------------------------------------------------------------------------

/// Sorts, by luma, every contiguous run of pixels whose luma lies in `[low, high]` (0–255) along
/// each row (or column when `vertical`) of `b`, and returns the permutation: for every pixel of
/// `src.rect` (row-major), the index of the source pixel that lands there. Pixels outside the band
/// (and outside `b`) stay put, which is what bounds the smear and keeps the picture legible.
fn sort_permutation(src: &Image, f: &PixelFormat, b: Rect, low: f32, high: f32, vertical: bool, reverse: bool) -> Option<Vec<u32>> {
    let w = src.rect.width() as usize;
    let h = src.rect.height() as usize;
    let count = w.checked_mul(h)?;
    u32::try_from(count).ok()?;
    let mut perm: Vec<u32> = (0..count as u32).collect();
    if b.is_empty() || w == 0 {
        return Some(perm);
    }
    let (lo, hi) = (low.min(high) / 255.0, low.max(high) / 255.0);
    let key = |x: i32, y: i32| {
        let l = luma(&rgba_at(src, f, x, y));
        if reverse { -l } else { l }
    };
    let in_band = |x: i32, y: i32| {
        let l = luma(&rgba_at(src, f, x, y));
        l >= lo && l <= hi
    };
    let (rx0, ry0) = (src.rect.x0, src.rect.y0);
    if vertical {
        // Columns are strided; sort compact copies and scatter them back.
        let cols: Vec<Vec<u32>> = par::map((b.x1 - b.x0) as usize, true, |cx| {
            let x = b.x0 + cx as i32;
            let mut line: Vec<u32> = (b.y0..b.y1).map(|y| ((y - ry0) as usize * w + (x - rx0) as usize) as u32).collect();
            let coords = |i: usize| (rx0 + (i % w) as i32, ry0 + (i / w) as i32);
            // Entries index the whole image, so the run sorter maps them back to coordinates.
            sort_line_by_entry(&mut line, &coords, &in_band, &key);
            line
        });
        for (cx, line) in cols.into_iter().enumerate() {
            let x = b.x0 + cx as i32;
            for (k, entry) in line.into_iter().enumerate() {
                let y = b.y0 + k as i32;
                if let Some(p) = perm.get_mut((y - ry0) as usize * w + (x - rx0) as usize) {
                    *p = entry;
                }
            }
        }
    } else {
        par::chunks_mut(&mut perm, w, true, |row, line| {
            let y = ry0 + row as i32;
            if y < b.y0 || y >= b.y1 {
                return;
            }
            let x0 = (b.x0 - rx0) as usize;
            let x1 = (b.x1 - rx0) as usize;
            if let Some(seg) = line.get_mut(x0..x1) {
                let coords = |i: usize| (rx0 + (i % w) as i32, ry0 + (i / w) as i32);
                sort_line_by_entry(seg, &coords, &in_band, &key);
            }
        });
    }
    Some(perm)
}

/// Sorts the in-band runs of `line`, whose entries are whole-image pixel indices in line order.
fn sort_line_by_entry(line: &mut [u32], coords: &dyn Fn(usize) -> (i32, i32), in_band: &dyn Fn(i32, i32) -> bool, key: &dyn Fn(i32, i32) -> f32) {
    let n = line.len();
    let mut start = None;
    for i in 0..=n {
        let inside = i < n
            && line.get(i).is_some_and(|&e| {
                let (x, y) = coords(e as usize);
                in_band(x, y)
            });
        match (start, inside) {
            (None, true) => start = Some(i),
            (Some(s), false) => {
                if let Some(run) = line.get_mut(s..i) {
                    run.sort_by(|&a, &c| {
                        let (ax, ay) = coords(a as usize);
                        let (cx, cy) = coords(c as usize);
                        key(ax, ay).total_cmp(&key(cx, cy))
                    });
                }
                start = None;
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Datamosh + Chromatic Aberration
// ---------------------------------------------------------------------------------------------

/// Datamosh: the image is cut into a macroblock grid; a fraction of blocks per row draw a random
/// motion vector and every other block inherits the accumulated vector of the block above, the way
/// P-frames keep applying their vectors when the I-frame is gone. Blocks are copied whole from the
/// source, so no pixel value is invented. Chromatic aberration then scales R out and B in about the
/// centre of the bounds (`aberration` is the fringe width at the far corner, in pixels). Alpha is
/// not fringed. Deterministic by `seed` *and block position*, so one seed lays the same glitch over
/// images of different sizes.
#[allow(clippy::too_many_arguments)]
pub(crate) fn datamosh(src: &Image, out: Rect, ctx: &Ctx, block: u32, amount: f32, drift: u32, aberration: f32, seed: u32) -> Vec<f32> {
    let n = src.ch;
    let mut res = identity(src, out);
    let b = ctx.bounds.intersect(&src.rect);
    let write = out.intersect(&b);
    if write.is_empty() || n == 0 {
        return res;
    }
    let f = fmt(ctx);
    let block = block.clamp(1, 4096) as i32;
    let (bw, bh) = (b.width() as i32, b.height() as i32);
    let nbx = (bw + block - 1) / block;
    let nby = (bh + block - 1) / block;
    let mosh = amount > 0.0 && drift > 0;
    // Accumulated motion vectors per block, row by row (the P-frame chain).
    let mut vx = vec![0i32; (nbx * nby) as usize];
    let mut vy = vec![0i32; (nbx * nby) as usize];
    if mosh {
        let d = drift.min(1 << 20) as i32;
        let yd = (d / 2).max(1);
        for i in 0..nby {
            for j in 0..nbx {
                let above = if i > 0 { ((i - 1) * nbx + j) as usize } else { usize::MAX };
                let (mut ax, mut ay) = (0, 0);
                if let (Some(px), Some(py)) = (vx.get(above), vy.get(above)) {
                    ax = *px;
                    ay = *py;
                }
                let fresh = hash01(j, i, 0, seed) < amount;
                if fresh {
                    ax = ax.saturating_add((hash01(j, i, 1, seed) * (2 * d + 1) as f32).floor() as i32 - d);
                    ay = ay.saturating_add((hash01(j, i, 2, seed) * (2 * yd + 1) as f32).floor() as i32 - yd);
                }
                let k = (i * nbx + j) as usize;
                if let (Some(px), Some(py)) = (vx.get_mut(k), vy.get_mut(k)) {
                    *px = ax;
                    *py = ay;
                }
            }
        }
    }
    // Where the moshed picture at `(x, y)` is read from in the source (clamped to the bounds).
    let moshed = |x: i32, y: i32| -> (i32, i32) {
        let x = x.clamp(b.x0, b.x1 - 1);
        let y = y.clamp(b.y0, b.y1 - 1);
        if !mosh {
            return (x, y);
        }
        let j = (x - b.x0) / block;
        let i = (y - b.y0) / block;
        let x0 = b.x0 + j * block;
        let y0 = b.y0 + i * block;
        let cw = block.min(b.x1 - x0);
        let ch = block.min(b.y1 - y0);
        let k = (i * nbx + j) as usize;
        let (dx, dy) = (vx.get(k).copied().unwrap_or(0), vy.get(k).copied().unwrap_or(0));
        let sx0 = x0.saturating_add(dx).clamp(b.x0, b.x1 - cw);
        let sy0 = y0.saturating_add(dy).clamp(b.y0, b.y1 - ch);
        (sx0 + (x - x0), sy0 + (y - y0))
    };
    let sample = |x: i32, y: i32| -> [f32; 4] {
        let (sx, sy) = moshed(x, y);
        rgba_at(src, &f, sx, sy)
    };
    // Chromatic aberration geometry.
    let cx = b.x0 as f32 + (bw - 1) as f32 / 2.0;
    let cy = b.y0 as f32 + (bh - 1) as f32 / 2.0;
    let rmax = ((bw - 1) as f32 / 2.0).hypot((bh - 1) as f32 / 2.0).max(1.0);
    let k = (aberration.max(0.0) / rmax).min(0.9);
    let fringe = aberration > 0.0 && k > 0.0;
    let radial = |x: i32, y: i32, scale: f32| -> (i32, i32) {
        let sx = (cx + (x as f32 - cx) / scale).floor() as i32;
        let sy = (cy + (y as f32 - cy) / scale).floor() as i32;
        (sx.clamp(b.x0, b.x1 - 1), sy.clamp(b.y0, b.y1 - 1))
    };
    for y in write.y0..write.y1 {
        for x in write.x0..write.x1 {
            let mut px = sample(x, y);
            if fringe {
                let (rx, ry) = radial(x, y, 1.0 + k);
                let (bx, by) = radial(x, y, 1.0 - k);
                px[0] = sample(rx, ry)[0];
                px[2] = sample(bx, by)[2];
            }
            put(&mut res, out, n, &f, x, y, px);
        }
    }
    res
}

// ---------------------------------------------------------------------------------------------
// Retro Console
// ---------------------------------------------------------------------------------------------

/// 4×4 Bayer threshold, centred in −0.5…0.5.
fn bayer4(x: i32, y: i32) -> f32 {
    const M: [[f32; 4]; 4] = [[0.0, 8.0, 2.0, 10.0], [12.0, 4.0, 14.0, 6.0], [3.0, 11.0, 1.0, 9.0], [15.0, 7.0, 13.0, 5.0]];
    let (i, j) = (y.rem_euclid(4) as usize, x.rem_euclid(4) as usize);
    (M.get(i).and_then(|r| r.get(j)).copied().unwrap_or(0.0) + 0.5) / 16.0 - 0.5
}

/// Retro Console (8-bit): blocks of `size` pixels (anchored at the bounds' origin, like Mosaic)
/// become their average colour, crushed to `levels` per channel, with an optional 4×4 ordered
/// dither whose threshold lands on the block grid so gradients break into the crosshatch of an
/// old console. Alpha is averaged and kept.
pub(crate) fn retro_console(src: &Image, out: Rect, ctx: &Ctx, size: u32, levels: u32, dither: bool) -> Vec<f32> {
    let n = src.ch;
    let mut res = identity(src, out);
    if out.is_empty() || n == 0 {
        return res;
    }
    let f = fmt(ctx);
    let cs = size.clamp(1, 4096) as i32;
    let b = ctx.bounds;
    let levels = levels.clamp(2, 256);
    let step = 1.0 / (levels - 1) as f32;
    let crush = |c: f32| ((c.clamp(0.0, 1.0) / step).round() * step).clamp(0.0, 1.0);
    let cx0 = (out.x0 - b.x0).div_euclid(cs);
    let cx1 = (out.x1 - 1 - b.x0).div_euclid(cs);
    let cy0 = (out.y0 - b.y0).div_euclid(cs);
    let cy1 = (out.y1 - 1 - b.y0).div_euclid(cs);
    for cy in cy0..=cy1 {
        for cx in cx0..=cx1 {
            let cell = Rect::new(b.x0 + cx * cs, b.y0 + cy * cs, b.x0 + (cx + 1) * cs, b.y0 + (cy + 1) * cs);
            let r = cell.intersect(&b).intersect(&src.rect);
            if r.is_empty() {
                continue;
            }
            // Alpha-weighted average, as Mosaic does, so transparent pixels don't darken a cell.
            let mut acc = [0.0f32; 4];
            let mut count = 0.0;
            for yy in r.y0..r.y1 {
                for xx in r.x0..r.x1 {
                    let p = rgba_at(src, &f, xx, yy);
                    acc[0] += p[0] * p[3];
                    acc[1] += p[1] * p[3];
                    acc[2] += p[2] * p[3];
                    acc[3] += p[3];
                    count += 1.0;
                }
            }
            if count <= 0.0 {
                continue;
            }
            let a = acc[3] / count;
            let mut px = if acc[3] > 1e-7 { [acc[0] / acc[3], acc[1] / acc[3], acc[2] / acc[3], a] } else { [0.0, 0.0, 0.0, a] };
            let bias = if dither { bayer4(cx, cy) * step } else { 0.0 };
            for c in px.iter_mut().take(3) {
                *c = crush(*c + bias);
            }
            let w = cell.intersect(&out);
            for y in w.y0..w.y1 {
                for x in w.x0..w.x1 {
                    put(&mut res, out, n, &f, x, y, px);
                }
            }
        }
    }
    res
}

// ---------------------------------------------------------------------------------------------
// Beam Dither
// ---------------------------------------------------------------------------------------------

/// One float plane over a rectangle (row-major).
struct Plane {
    w: usize,
    h: usize,
    v: Vec<f32>,
}

impl Plane {
    fn new(w: usize, h: usize) -> Option<Plane> {
        let len = w.checked_mul(h)?;
        if len > (1usize << 31) {
            return None;
        }
        Some(Plane { w, h, v: vec![0.0; len] })
    }
    #[inline]
    fn get(&self, x: usize, y: usize) -> f32 {
        self.v.get(y * self.w + x).copied().unwrap_or(0.0)
    }
    fn map(&mut self, f: impl Fn(f32) -> f32) {
        for v in &mut self.v {
            *v = f(*v);
        }
    }
}

/// Three passes of a `2·r + 1` box in each direction (≈ Gaussian), edges clamped.
fn box_blur3(p: &Plane, r: usize) -> Option<Plane> {
    if r == 0 || p.w == 0 || p.h == 0 {
        return Some(Plane { w: p.w, h: p.h, v: p.v.clone() });
    }
    let mut cur = Plane { w: p.w, h: p.h, v: p.v.clone() };
    let mut tmp = Plane::new(p.w, p.h)?;
    let k = (2 * r + 1) as f32;
    for _ in 0..3 {
        // Horizontal: running sum along each row.
        par::chunks2_mut(&mut tmp.v, p.w, &mut cur.v, p.w, true, |_, dst, row| {
            let w = row.len();
            let at = |x: isize| row.get(x.clamp(0, w as isize - 1) as usize).copied().unwrap_or(0.0);
            let mut sum = 0.0;
            for dx in -(r as isize)..=(r as isize) {
                sum += at(dx);
            }
            for (x, d) in dst.iter_mut().enumerate() {
                *d = sum / k;
                sum += at(x as isize + r as isize + 1) - at(x as isize - r as isize);
            }
        });
        // Vertical: column running sums over the whole plane (rows are strided).
        let w = p.w;
        let h = p.h;
        let src = &tmp.v;
        let dst = &mut cur.v;
        let at = |x: usize, y: isize| src.get(y.clamp(0, h as isize - 1) as usize * w + x).copied().unwrap_or(0.0);
        let mut sums: Vec<f32> = (0..w).map(|x| (-(r as isize)..=(r as isize)).map(|dy| at(x, dy)).sum()).collect();
        for y in 0..h {
            for x in 0..w {
                if let Some(d) = dst.get_mut(y * w + x) {
                    *d = sums.get(x).copied().unwrap_or(0.0) / k;
                }
                if let Some(s) = sums.get_mut(x) {
                    *s += at(x, y as isize + r as isize + 1) - at(x, y as isize - r as isize);
                }
            }
        }
    }
    Some(cur)
}

/// Levels, blur, sharpen and grain, in the order a darkroom would: sharpen after blur so the pair
/// is a band-pass, grain last so the dither sees it as signal. `origin` seeds the grain by
/// document position, so tiles and re-renders agree.
fn condition(plane: Plane, spec: &BeamDitherSpec, origin: (i32, i32)) -> Option<Plane> {
    let mut p = plane;
    let brightness = spec.brightness.clamp(-100.0, 100.0) / 100.0;
    let contrast = spec.contrast.clamp(-100.0, 100.0) / 100.0;
    p.map(|v| (v + brightness).clamp(0.0, 1.0));
    if contrast != 0.0 {
        // Pivot on mid grey so contrast opens the range without shifting its centre.
        let gain = if contrast > 0.0 { 1.0 + contrast } else { 1.0 / (1.0 - contrast) };
        p.map(|v| ((v - 0.5) * gain + 0.5).clamp(0.0, 1.0));
    }
    let blur = spec.blur.clamp(0.0, 20.0).round() as usize;
    if blur > 0 {
        p = box_blur3(&p, blur)?;
    }
    let sharpen = spec.sharpen.clamp(0.0, 300.0) / 100.0;
    if sharpen > 0.0 {
        let radius = spec.sharpen_radius.clamp(1.0, 20.0).round() as usize;
        let low = box_blur3(&p, radius)?;
        for (v, l) in p.v.iter_mut().zip(low.v.iter()) {
            *v = (*v + sharpen * (*v - *l)).clamp(0.0, 1.0);
        }
    }
    let noise = spec.noise.clamp(-100.0, 100.0);
    if noise > 0.0 {
        let w = p.w;
        for (i, v) in p.v.iter_mut().enumerate() {
            let (x, y) = ((i % w) as i32, (i / w) as i32);
            let g = hash01(origin.0.wrapping_add(x), origin.1.wrapping_add(y), 0, 0x0D17) * 2.0 - 1.0;
            *v = (*v + g * noise / 200.0).clamp(0.0, 1.0);
        }
    } else if noise < 0.0 {
        p = box_blur3(&p, ((-noise / 20.0) as usize).max(1))?;
    }
    Some(p)
}

/// Block-average down by `factor` (the trailing partial cell is dropped, as the upsample pads it).
fn downsample(p: &Plane, factor: usize) -> Option<Plane> {
    if factor <= 1 {
        return Some(Plane { w: p.w, h: p.h, v: p.v.clone() });
    }
    let (rows, cols) = (p.h / factor, p.w / factor);
    if rows == 0 || cols == 0 {
        return Some(Plane { w: p.w, h: p.h, v: p.v.clone() });
    }
    let mut small = Plane::new(cols, rows)?;
    let inv = 1.0 / (factor * factor) as f32;
    for cy in 0..rows {
        for cx in 0..cols {
            let mut s = 0.0;
            for y in cy * factor..(cy + 1) * factor {
                for x in cx * factor..(cx + 1) * factor {
                    s += p.get(x, y);
                }
            }
            if let Some(v) = small.v.get_mut(cy * cols + cx) {
                *v = s * inv;
            }
        }
    }
    Some(small)
}

/// Nearest-neighbour back up to `(w, h)`; the trailing partial cell repeats the edge. Nearest,
/// never smooth: interpolation would reintroduce the tones the dither just removed.
fn upsample(p: &Plane, factor: usize, w: usize, h: usize) -> Option<Plane> {
    if p.w == w && p.h == h {
        return Some(Plane { w, h, v: p.v.clone() });
    }
    let mut big = Plane::new(w, h)?;
    let f = factor.max(1);
    for y in 0..h {
        let sy = (y / f).min(p.h.saturating_sub(1));
        for x in 0..w {
            let sx = (x / f).min(p.w.saturating_sub(1));
            if let Some(v) = big.v.get_mut(y * w + x) {
                *v = p.get(sx, sy);
            }
        }
    }
    Some(big)
}

/// Snap 0…1 onto `levels` evenly spaced tones.
fn quantise(v: f32, levels: u32) -> f32 {
    let steps = (levels.max(2) - 1) as f32;
    ((v * steps).round() / steps).clamp(0.0, 1.0)
}

/// The recursive Bayer matrix of `size` (2, 4 or 8), normalised to 0…1 exclusive.
fn bayer_threshold(size: usize, x: usize, y: usize) -> f32 {
    let size = size.clamp(2, 8).next_power_of_two();
    (bayer_exact(size, x % size, y % size) as f32 + 0.5) / (size * size) as f32
}

/// Bayer entry by the doubling recurrence `M(2n) = [[4M, 4M+2], [4M+3, 4M+1]]` from `[[0, 2], [3, 1]]`.
fn bayer_exact(size: usize, x: usize, y: usize) -> u32 {
    if size <= 2 {
        return match (y % 2, x % 2) {
            (0, 0) => 0,
            (0, _) => 2,
            (_, 0) => 3,
            _ => 1,
        };
    }
    let half = size / 2;
    let inner = bayer_exact(half, x % half, y % half);
    let q = match (y / half, x / half) {
        (0, 0) => 0,
        (0, _) => 2,
        (_, 0) => 3,
        _ => 1,
    };
    4 * inner + q
}

/// Ordered dither: the threshold is applied inside one quantisation step, so it dithers between
/// adjacent tones rather than only between black and white.
fn ordered_dither(p: &Plane, size: usize, levels: u32) -> Plane {
    let steps = (levels.max(2) - 1) as f32;
    let mut out = Plane { w: p.w, h: p.h, v: p.v.clone() };
    let w = p.w;
    for (i, v) in out.v.iter_mut().enumerate() {
        let t = bayer_threshold(size, i % w, i / w);
        *v = ((*v * steps + (t - 0.5)).round() / steps).clamp(0.0, 1.0);
    }
    out
}

/// Error-diffusion kernels as the literature writes them: `(dx, dy, weight)` and the divisor.
fn diffusion_kernel(alg: DitherAlgorithm) -> (&'static [(i32, i32, f32)], f32) {
    match alg {
        DitherAlgorithm::Atkinson => (&[(1, 0, 1.0), (2, 0, 1.0), (-1, 1, 1.0), (0, 1, 1.0), (1, 1, 1.0), (0, 2, 1.0)], 8.0),
        DitherAlgorithm::Jarvis => (
            &[
                (1, 0, 7.0),
                (2, 0, 5.0),
                (-2, 1, 3.0),
                (-1, 1, 5.0),
                (0, 1, 7.0),
                (1, 1, 5.0),
                (2, 1, 3.0),
                (-2, 2, 1.0),
                (-1, 2, 3.0),
                (0, 2, 5.0),
                (1, 2, 3.0),
                (2, 2, 1.0),
            ],
            48.0,
        ),
        DitherAlgorithm::Stucki => (
            &[
                (1, 0, 8.0),
                (2, 0, 4.0),
                (-2, 1, 2.0),
                (-1, 1, 4.0),
                (0, 1, 8.0),
                (1, 1, 4.0),
                (2, 1, 2.0),
                (-2, 2, 1.0),
                (-1, 2, 2.0),
                (0, 2, 4.0),
                (1, 2, 2.0),
                (2, 2, 1.0),
            ],
            42.0,
        ),
        DitherAlgorithm::Sierra => {
            (&[(1, 0, 5.0), (2, 0, 3.0), (-2, 1, 2.0), (-1, 1, 4.0), (0, 1, 5.0), (1, 1, 4.0), (2, 1, 2.0), (-1, 2, 2.0), (0, 2, 3.0), (1, 2, 2.0)], 32.0)
        }
        DitherAlgorithm::Burkes => (&[(1, 0, 8.0), (2, 0, 4.0), (-2, 1, 2.0), (-1, 1, 4.0), (0, 1, 8.0), (1, 1, 4.0), (2, 1, 2.0)], 32.0),
        _ => (&[(1, 0, 7.0), (-1, 1, 3.0), (0, 1, 5.0), (1, 1, 1.0)], 16.0),
    }
}

/// Serpentine error diffusion (alternate rows run right to left, which cancels the diagonal
/// worming a fixed scan direction prints). Sequential by nature: a pixel can't be quantised until
/// its neighbour has pushed error into it.
fn error_diffuse(p: &Plane, alg: DitherAlgorithm, levels: u32) -> Plane {
    let (offsets, divisor) = diffusion_kernel(alg);
    let (w, h) = (p.w, p.h);
    let mut work: Vec<f64> = p.v.iter().map(|&v| f64::from(v)).collect();
    let mut out = Plane { w, h, v: vec![0.0; p.v.len()] };
    let steps = f64::from(levels.max(2) - 1);
    for y in 0..h {
        let rightwards = y % 2 == 0;
        let sign: i32 = if rightwards { 1 } else { -1 };
        for k in 0..w {
            let x = if rightwards { k } else { w - 1 - k };
            let old = work.get(y * w + x).copied().unwrap_or(0.0);
            let new = ((old * steps).round() / steps).clamp(0.0, 1.0);
            if let Some(o) = out.v.get_mut(y * w + x) {
                *o = new as f32;
            }
            let err = old - new;
            if err == 0.0 {
                continue;
            }
            for &(dx, dy, wgt) in offsets {
                let ny = y as i64 + i64::from(dy);
                let nx = x as i64 + i64::from(dx * sign);
                if ny >= h as i64 || nx < 0 || nx >= w as i64 {
                    continue;
                }
                if let Some(t) = work.get_mut(ny as usize * w + nx as usize) {
                    *t += err * f64::from(wgt) / f64::from(divisor);
                }
            }
        }
    }
    out
}

/// Beam modulation: a raster of horizontal beams whose phase is advanced by the luminance (so
/// bright regions bend their beam off its resting line) and whose lit width is set by it (so beams
/// thicken into solid white in highlights and thin to dashes in shadow). Returns coverage 0…1.
fn beam_mask(p: &Plane, pitch: u32, amplitude: f32) -> Plane {
    let pitch = f64::from(pitch.max(2));
    let amplitude = f64::from(amplitude.clamp(0.0, 8.0));
    let edge = 0.12f64;
    let mut out = Plane { w: p.w, h: p.h, v: vec![0.0; p.v.len()] };
    let w = p.w;
    for (i, o) in out.v.iter_mut().enumerate() {
        let lum = f64::from(p.v.get(i).copied().unwrap_or(0.0));
        let row = (i / w) as f64;
        let phase = row / pitch + amplitude * lum;
        let profile = 0.5 + 0.5 * (2.0 * std::f64::consts::PI * phase).cos();
        // The threshold runs from 1 down to −edge so pure white can close the gaps between beams.
        let level = (1.0 - lum) * (1.0 + edge) - edge;
        *o = ((profile - level) / edge).clamp(0.0, 1.0) as f32;
    }
    out
}

fn render(small: &Plane, spec: &BeamDitherSpec) -> Plane {
    let levels = spec.levels.clamp(2, 8);
    match spec.algorithm {
        DitherAlgorithm::Beam => {
            let coverage = beam_mask(small, spec.beam_pitch, spec.beam_amplitude);
            // Dithering the beam's soft shoulders keeps the render honestly two-tone; above 2
            // levels the beam keeps its gradient and is merely posterised.
            if levels <= 2 {
                ordered_dither(&coverage, 4, 2)
            } else {
                let mut c = coverage;
                c.map(|v| quantise(v, levels));
                c
            }
        }
        DitherAlgorithm::Bayer2 => ordered_dither(small, 2, levels),
        DitherAlgorithm::Bayer4 => ordered_dither(small, 4, levels),
        DitherAlgorithm::Bayer8 => ordered_dither(small, 8, levels),
        DitherAlgorithm::Threshold => {
            let mut c = Plane { w: small.w, h: small.h, v: small.v.clone() };
            c.map(|v| quantise(v, levels));
            c
        }
        other => error_diffuse(small, other, levels),
    }
}

/// Condition → downsample → render → upsample one plane over the bounds.
fn render_plane(plane: Plane, spec: &BeamDitherSpec, origin: (i32, i32)) -> Option<(Plane, Plane)> {
    let (w, h) = (plane.w, plane.h);
    let conditioned = condition(plane, spec, origin)?;
    let scale = spec.scale.clamp(1, 32) as usize;
    let small = downsample(&conditioned, scale)?;
    let tone = render(&small, spec);
    let big = upsample(&tone, scale, w, h)?;
    Some((big, conditioned))
}

/// The rendered Beam Dither over the bounds: tone planes plus the conditioned luma for tri-tone.
pub(crate) struct Beam {
    rect: Rect,
    /// One plane (mono / tonal) or three (color).
    planes: Vec<Plane>,
    luma: Option<Plane>,
    spec: BeamDitherSpec,
}

impl Beam {
    fn new(src: &Image, ctx: &Ctx, spec: &BeamDitherSpec) -> Option<Beam> {
        let b = ctx.bounds.intersect(&src.rect);
        let (w, h) = (b.width() as usize, b.height() as usize);
        let f = fmt(ctx);
        let origin = (b.x0, b.y0);
        let read = |c: Option<usize>| -> Option<Plane> {
            let mut p = Plane::new(w, h)?;
            for y in 0..h {
                for x in 0..w {
                    let px = rgba_at(src, &f, b.x0 + x as i32, b.y0 + y as i32);
                    let v = match c {
                        Some(i) => px.get(i).copied().unwrap_or(0.0),
                        None => luma(&px),
                    };
                    if let Some(o) = p.v.get_mut(y * w + x) {
                        *o = v;
                    }
                }
            }
            Some(p)
        };
        let mut beam = Beam { rect: b, planes: Vec::new(), luma: None, spec: spec.clone() };
        if spec.mode == BeamMode::Color {
            for c in 0..3 {
                let (tone, _) = render_plane(read(Some(c))?, spec, origin)?;
                beam.planes.push(tone);
            }
        } else {
            let (tone, conditioned) = render_plane(read(None)?, spec, origin)?;
            beam.planes.push(tone);
            beam.luma = Some(conditioned);
        }
        Some(beam)
    }

    /// Ink the rendered tone at `(x, y)` (inside the bounds).
    fn rgb(&self, x: usize, y: usize) -> [f32; 3] {
        let spec = &self.spec;
        match spec.mode {
            BeamMode::Color => {
                let c = |i: usize| self.planes.get(i).map_or(0.0, |p| p.get(x, y));
                [c(0), c(1), c(2)]
            }
            mode => {
                let tone = self.planes.first().map_or(0.0, |p| p.get(x, y));
                let ink = if mode == BeamMode::Tonal {
                    // Which ink a lit pixel takes is decided by the *original* luminance under it,
                    // so shadows keep their own colour where the dither happens to light them.
                    let l = self.luma.as_ref().map_or(0.0, |p| p.get(x, y));
                    if l < 1.0 / 3.0 {
                        spec.shadows
                    } else if l < 2.0 / 3.0 {
                        spec.midtones
                    } else {
                        spec.highlights
                    }
                } else {
                    spec.highlights
                };
                let bg = spec.background;
                [bg[0] + (ink[0] - bg[0]) * tone, bg[1] + (ink[1] - bg[1]) * tone, bg[2] + (ink[2] - bg[2]) * tone]
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Prepared global work
// ---------------------------------------------------------------------------------------------

/// Whole-image work of a global filter, built once per run from the shared bounds image and
/// gathered from per tile. [`kernel`](crate::kernel) builds it per call when it has no shared one.
pub(crate) enum Prepared {
    /// Nothing to do (empty bounds, degenerate params): the identity.
    Identity,
    /// Pixel Sort: for each pixel of `rect` (row-major) the source pixel index that lands there.
    Permutation { rect: Rect, perm: Vec<u32> },
    /// Film Negative: per-channel development bounds.
    Negative(crate::darkroom::Negative),
    /// Beam Dither: the rendered tone planes.
    Beam(Box<Beam>),
}

impl Prepared {
    /// `None` for filters that aren't prepared this way.
    pub(crate) fn new(params: &FilterParams, src: &Image, ctx: &Ctx) -> Option<Prepared> {
        Some(match params {
            FilterParams::PixelSort { low, high, vertical, reverse } => {
                let b = ctx.bounds.intersect(&src.rect);
                match sort_permutation(src, &fmt(ctx), b, *low, *high, *vertical, *reverse) {
                    Some(perm) => Prepared::Permutation { rect: src.rect, perm },
                    None => Prepared::Identity,
                }
            }
            FilterParams::FilmNegative { mode, clip } => match crate::darkroom::Negative::new(src, ctx, *mode, *clip) {
                Some(n) => Prepared::Negative(n),
                None => Prepared::Identity,
            },
            FilterParams::BeamDither { spec } => match Beam::new(src, ctx, spec) {
                Some(b) => Prepared::Beam(Box::new(b)),
                None => Prepared::Identity,
            },
            _ => return None,
        })
    }

    /// Whether `params` is a filter that [`Prepared::new`] handles.
    pub(crate) fn handles(params: &FilterParams) -> bool {
        matches!(params, FilterParams::PixelSort { .. } | FilterParams::FilmNegative { .. } | FilterParams::BeamDither { .. })
    }

    /// The samples of `out`.
    pub(crate) fn run(&self, src: &Image, out: Rect, ctx: &Ctx) -> Vec<f32> {
        let n = src.ch;
        let mut res = identity(src, out);
        if out.is_empty() || n == 0 {
            return res;
        }
        let f = fmt(ctx);
        match self {
            Prepared::Identity => {}
            Prepared::Permutation { rect, perm } => {
                let w = rect.width() as usize;
                let write = out.intersect(rect);
                for y in write.y0..write.y1 {
                    for x in write.x0..write.x1 {
                        let i = (y - rect.y0) as usize * w + (x - rect.x0) as usize;
                        let Some(&s) = perm.get(i) else { continue };
                        let s = s as usize;
                        let (sx, sy) = (rect.x0 + (s % w) as i32, rect.y0 + (s / w) as i32);
                        if !src.rect.contains(sx, sy) {
                            continue;
                        }
                        let d = ((y - out.y0) as usize * out.width() as usize + (x - out.x0) as usize) * n;
                        if let Some(dst) = res.get_mut(d..d + n) {
                            dst.copy_from_slice(src.px(sx, sy));
                        }
                    }
                }
            }
            Prepared::Negative(neg) => {
                let write = out.intersect(&ctx.bounds).intersect(&src.rect);
                for y in write.y0..write.y1 {
                    for x in write.x0..write.x1 {
                        let px = rgba_at(src, &f, x, y);
                        put(&mut res, out, n, &f, x, y, neg.develop(px));
                    }
                }
            }
            Prepared::Beam(beam) => {
                let write = out.intersect(&beam.rect).intersect(&src.rect);
                for y in write.y0..write.y1 {
                    for x in write.x0..write.x1 {
                        let a = rgba_at(src, &f, x, y)[3];
                        let rgb = beam.rgb((x - beam.rect.x0) as usize, (y - beam.rect.y0) as usize);
                        put(&mut res, out, n, &f, x, y, [rgb[0], rgb[1], rgb[2], a]);
                    }
                }
            }
        }
        res
    }
}

/// [`kernel`](crate::kernel) entry for the prepared global filters: builds the preparation from
/// `src` (the bounds image) and gathers `out`. Correct whatever the tiling, just repeated per tile.
pub(crate) fn run_prepared(params: &FilterParams, src: &Image, out: Rect, ctx: &Ctx) -> Vec<f32> {
    match Prepared::new(params, src, ctx) {
        Some(p) => p.run(src, out, ctx),
        None => identity(src, out),
    }
}
