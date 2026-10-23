//! Behaviour tests for the Photoslop ports: Sepia, Chroma Denoise, Film Negative, Pixel Sort,
//! Datamosh, Retro Console and Beam Dither. Tile independence, selection mixing and serde
//! round-trips are covered by `tests.rs` through `all_filters()`.

use super::*;
use photocraft_color::{PixelFormat, SampleType};
use photocraft_raster::{from_rgba, to_rgba};

const B: Rect = Rect { x0: 0, y0: 0, x1: 40, y1: 30 };

fn rgba_f32() -> PixelFormat {
    PixelFormat::new(ColorMode::Rgb, SampleType::F32, true)
}

/// A surface over `r` from a straight-RGBA generator, in any format.
fn surface(fmt: PixelFormat, r: Rect, f: impl Fn(i32, i32) -> [f32; 4]) -> Surface {
    let mut s = Surface::new(fmt);
    let mut v = Vec::new();
    for y in r.y0..r.y1 {
        for x in r.x0..r.x1 {
            v.extend(from_rgba(&fmt, f(x, y)));
        }
    }
    s.write_region(r, &v);
    s
}

/// Straight RGBA of every pixel of `r`, row-major.
fn pixels(s: &Surface, r: Rect) -> Vec<[f32; 4]> {
    let f = s.format();
    s.read_region(r).chunks_exact(f.channels()).map(|px| to_rgba(&f, px)).collect()
}

fn luma(c: &[f32; 4]) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

/// Deterministic pseudo-random colour (no clipping risk: mid-range).
fn noisy(x: i32, y: i32) -> [f32; 4] {
    let h = |k: u32| noise::hash01(x, y, k, 11);
    [0.4 + 0.2 * h(0), 0.4 + 0.2 * h(1), 0.4 + 0.2 * h(2), 1.0]
}

fn run(s: &Surface, p: &FilterParams) -> Surface {
    let area = output_area(p, s.content_bounds(), B, None);
    apply(s, p, area, B, None)
}

fn sorted(mut v: Vec<[f32; 4]>) -> Vec<[f32; 4]> {
    v.sort_by(|a, b| a.iter().zip(b.iter()).map(|(x, y)| x.total_cmp(y)).find(|o| o.is_ne()).unwrap_or(std::cmp::Ordering::Equal));
    v
}

#[test]
fn pixel_sort_sorts_whole_rows_and_keeps_every_pixel() {
    let s = surface(rgba_f32(), B, noisy);
    let before = pixels(&s, B);
    let out = run(&s, &FilterParams::PixelSort { low: 0.0, high: 255.0, vertical: false, reverse: false });
    let after = pixels(&out, B);
    let w = B.width() as usize;
    for y in 0..B.height() as usize {
        let row = &after[y * w..(y + 1) * w];
        assert!(row.windows(2).all(|p| luma(&p[0]) <= luma(&p[1]) + 1e-6), "row {y} is not sorted dark→bright");
        assert_eq!(sorted(row.to_vec()), sorted(before[y * w..(y + 1) * w].to_vec()), "row {y} invented or lost pixels");
    }
    assert_ne!(before, after);
}

#[test]
fn pixel_sort_columns_reverse_is_bright_first() {
    let s = surface(rgba_f32(), B, noisy);
    let out = run(&s, &FilterParams::PixelSort { low: 0.0, high: 255.0, vertical: true, reverse: true });
    let after = pixels(&out, B);
    let w = B.width() as usize;
    for x in 0..w {
        let col: Vec<[f32; 4]> = (0..B.height() as usize).map(|y| after[y * w + x]).collect();
        assert!(col.windows(2).all(|p| luma(&p[0]) + 1e-6 >= luma(&p[1])), "column {x} is not sorted bright→dark");
    }
}

#[test]
fn pixel_sort_leaves_out_of_band_pixels_in_place() {
    // A hard band: luma 0.45..0.55 (≈115..140 of 255). Everything outside must not move.
    let s = surface(rgba_f32(), B, noisy);
    let before = pixels(&s, B);
    let (lo, hi) = (0.45 * 255.0, 0.55 * 255.0);
    let out = run(&s, &FilterParams::PixelSort { low: lo, high: hi, vertical: false, reverse: false });
    let after = pixels(&out, B);
    let mut moved = 0;
    for (b, a) in before.iter().zip(after.iter()) {
        let l = luma(b) * 255.0;
        if l < lo || l > hi {
            assert_eq!(a, b, "an out-of-band pixel moved");
        } else if a != b {
            moved += 1;
        }
    }
    assert!(moved > 0, "nothing in the band was sorted");
}

#[test]
fn datamosh_is_deterministic_by_seed_and_aberration_keeps_green_and_alpha() {
    let s = surface(rgba_f32(), B, noisy);
    let p = |seed, aberration| FilterParams::Datamosh { block: 4, amount: 0.5, drift: 6, aberration, seed };
    let a = pixels(&run(&s, &p(7, 2.0)), B);
    let b = pixels(&run(&s, &p(7, 2.0)), B);
    let c = pixels(&run(&s, &p(8, 2.0)), B);
    assert_eq!(a, b, "same seed, different mosh");
    assert_ne!(a, c, "seed has no effect");
    // Fringe alone: green and alpha are never shifted; red is.
    let before = pixels(&s, B);
    let fringe = pixels(&run(&s, &FilterParams::Datamosh { block: 16, amount: 0.0, drift: 0, aberration: 4.0, seed: 1 }), B);
    assert!(before.iter().zip(fringe.iter()).all(|(x, y)| x[1] == y[1] && x[3] == y[3]));
    assert!(before.iter().zip(fringe.iter()).any(|(x, y)| x[0] != y[0]));
    // Blocks are copied whole, so a mosh without fringe invents no pixel value.
    let moshed = pixels(&run(&s, &p(7, 0.0)), B);
    assert_ne!(moshed, before);
    for px in &moshed {
        assert!(before.contains(px), "{px:?} is not a source pixel");
    }
}

#[test]
fn retro_console_snaps_colours_to_the_level_grid_and_keeps_alpha() {
    let s = surface(rgba_f32(), B, |x, y| {
        let c = noisy(x, y);
        [c[0], c[1], c[2], if x < 10 { 0.5 } else { 1.0 }]
    });
    for dither in [false, true] {
        let out = run(&s, &FilterParams::RetroConsole { size: 3, levels: 4, dither });
        for (i, px) in pixels(&out, B).iter().enumerate() {
            for c in &px[..3] {
                let k = (c * 3.0).round() / 3.0;
                assert!((c - k).abs() < 1e-5, "pixel {i}: {c} is off the 4-level grid (dither={dither})");
            }
            let x = (i % B.width() as usize) as i32;
            let a = if x < 10 { 0.5 } else { 1.0 };
            // Cells are 3 px wide anchored at 0; the cell spanning 9..12 mixes alphas.
            if !(9..12).contains(&x) {
                assert!((px[3] - a).abs() < 1e-5, "alpha was not kept");
            }
        }
    }
}

#[test]
fn film_negative_develops_a_colour_negative_through_its_orange_mask() {
    // Positive P: a grey ramp with a colour gradient. Negative scan: v_c = mask_c / (1 + 9·P_c),
    // i.e. transmittance falling with exposure under a per-channel dye mask.
    let positive = |x: i32, y: i32| [x as f32 / 39.0, y as f32 / 29.0, ((x + y) % 20) as f32 / 19.0, 1.0];
    let mask = [1.0f32, 0.7, 0.45];
    let s = surface(rgba_f32(), B, |x, y| {
        let p = positive(x, y);
        [mask[0] / (1.0 + 9.0 * p[0]), mask[1] / (1.0 + 9.0 * p[1]), mask[2] / (1.0 + 9.0 * p[2]), 1.0]
    });
    for mode in [NegativeMode::Auto, NegativeMode::Color] {
        let out = pixels(&run(&s, &FilterParams::FilmNegative { mode, clip: 0.0 }), B);
        for (i, px) in out.iter().enumerate() {
            let (x, y) = ((i % 40) as i32, (i / 40) as i32);
            let p = positive(x, y);
            for c in 0..3 {
                assert!((px[c] - p[c]).abs() < 0.06, "({x},{y}) channel {c}: developed {} vs positive {} ({mode:?})", px[c], p[c]);
            }
        }
    }
    // Plain inversion would leave the mask in: the brightest developed red must not be dim.
    let plain: Vec<f32> = pixels(&s, B).iter().map(|c| 1.0 - c[2]).collect();
    assert!(plain.iter().cloned().fold(0.0, f32::max) < 0.96, "test negative has no mask to remove");
}

#[test]
fn film_negative_keeps_a_mono_negative_neutral() {
    let s = surface(rgba_f32(), B, |x, _| {
        let v = 0.9 / (1.0 + 9.0 * x as f32 / 39.0);
        [v * 1.02, v, v * 0.98, 1.0] // a slight scanner tint
    });
    let out = pixels(&run(&s, &FilterParams::FilmNegative { mode: NegativeMode::Auto, clip: 0.0 }), B);
    for px in &out {
        assert!((px[0] - px[1]).abs() < 1e-6 && (px[1] - px[2]).abs() < 1e-6, "mono negative developed with a cast: {px:?}");
    }
    // Thin negative (left, high scan value) → black; dense negative (right) → bright positive.
    assert!(out[0][0] < 0.05 && out[39][0] > 0.95, "{} {}", out[0][0], out[39][0]);
    assert!(out.windows(2).take(39).all(|p| p[0][0] <= p[1][0]));
}

#[test]
fn beam_dither_renders_two_tones_and_tri_tone_inks() {
    let s = surface(rgba_f32(), B, |x, y| {
        let v = (x as f32 / 39.0 + y as f32 / 29.0) / 2.0;
        [v, v, v, 1.0]
    });
    for algorithm in [
        DitherAlgorithm::Beam,
        DitherAlgorithm::FloydSteinberg,
        DitherAlgorithm::Atkinson,
        DitherAlgorithm::Jarvis,
        DitherAlgorithm::Stucki,
        DitherAlgorithm::Sierra,
        DitherAlgorithm::Burkes,
        DitherAlgorithm::Bayer2,
        DitherAlgorithm::Bayer4,
        DitherAlgorithm::Bayer8,
        DitherAlgorithm::Threshold,
    ] {
        let spec = BeamDitherSpec { algorithm, scale: 1, ..BeamDitherSpec::default() };
        let out = pixels(&run(&s, &FilterParams::BeamDither { spec }), B);
        let mut lit = 0;
        for px in &out {
            assert!(px[0] == px[1] && px[1] == px[2], "{algorithm:?}: mono render is tinted");
            assert!(px[0] == 0.0 || px[0] == 1.0, "{algorithm:?}: {} is not two-tone", px[0]);
            lit += usize::from(px[0] == 1.0);
        }
        assert!(lit > 0 && lit < out.len(), "{algorithm:?}: flat render");
    }
    // Tri-tone: lit pixels wear the ink of the original tone under them.
    let inks = ([0.2, 0.3, 0.9, 1.0], [0.5, 0.6, 0.1, 1.0], [1.0, 0.8, 0.2, 1.0]);
    let spec = BeamDitherSpec {
        algorithm: DitherAlgorithm::Bayer4,
        mode: BeamMode::Tonal,
        scale: 1,
        shadows: inks.0,
        midtones: inks.1,
        highlights: inks.2,
        background: [0.0, 0.0, 0.0, 1.0],
        ..BeamDitherSpec::default()
    };
    let out = pixels(&run(&s, &FilterParams::BeamDither { spec }), B);
    let mut seen = [false; 3];
    for px in &out {
        let rgb = [px[0], px[1], px[2]];
        if rgb == [0.0, 0.0, 0.0] {
            continue;
        }
        let which = [inks.0, inks.1, inks.2].iter().position(|ink| (0..3).all(|c| (ink[c] - rgb[c]).abs() < 1e-6));
        let which = which.unwrap_or_else(|| panic!("{rgb:?} is not one of the three inks"));
        seen[which] = true;
    }
    assert_eq!(seen, [true; 3], "a gradient should light all three inks");
    // Colour mode dithers each channel on its own.
    let spec = BeamDitherSpec { algorithm: DitherAlgorithm::FloydSteinberg, mode: BeamMode::Color, scale: 1, ..BeamDitherSpec::default() };
    let out = pixels(&run(&surface(rgba_f32(), B, noisy), &FilterParams::BeamDither { spec }), B);
    assert!(out.iter().all(|px| px[..3].iter().all(|c| *c == 0.0 || *c == 1.0)));
    assert!(out.iter().any(|px| px[0] != px[1]));
}

#[test]
fn chroma_denoise_preserves_luma_and_tames_a_speckle() {
    let s = surface(rgba_f32(), B, noisy);
    let before = pixels(&s, B);
    let after = pixels(&run(&s, &FilterParams::ChromaDenoise { strength: 40.0 }), B);
    for (b, a) in before.iter().zip(after.iter()) {
        assert!((luma(b) - luma(a)).abs() < 2e-3, "luma moved: {} → {}", luma(b), luma(a));
        assert_eq!(b[3], a[3]);
    }
    assert_ne!(before, after);
    let speck = surface(rgba_f32(), B, |x, y| if (x, y) == (20, 15) { [0.9, 0.3, 0.3, 1.0] } else { [0.5, 0.5, 0.5, 1.0] });
    let out = pixels(&run(&speck, &FilterParams::ChromaDenoise { strength: 30.0 }), B);
    let px = out[15 * 40 + 20];
    assert!((px[0] - px[1]).abs() < 0.3, "speckle chroma not reduced: {px:?}");
}

#[test]
fn sepia_is_identity_at_zero_and_warm_at_full() {
    let s = surface(rgba_f32(), B, noisy);
    assert_eq!(pixels(&run(&s, &FilterParams::Sepia { amount: 0.0 }), B), pixels(&s, B));
    let white = surface(rgba_f32(), B, |_, _| [1.0; 4]);
    let out = pixels(&run(&white, &FilterParams::Sepia { amount: 100.0 }), B);
    let px = out[0];
    assert!((px[0] - 1.0).abs() < 1e-6 && (px[1] - 0.89).abs() < 1e-5 && (px[2] - 0.62).abs() < 1e-5, "{px:?}");
}

#[test]
fn ports_run_at_every_depth_and_colour_model() {
    let params = [
        FilterParams::Sepia { amount: 60.0 },
        FilterParams::ChromaDenoise { strength: 25.0 },
        FilterParams::FilmNegative { mode: NegativeMode::Auto, clip: 0.5 },
        FilterParams::PixelSort { low: 30.0, high: 230.0, vertical: false, reverse: false },
        FilterParams::Datamosh { block: 4, amount: 0.4, drift: 5, aberration: 1.5, seed: 3 },
        FilterParams::RetroConsole { size: 3, levels: 4, dither: true },
        FilterParams::BeamDither { spec: BeamDitherSpec::default() },
    ];
    for sample in [SampleType::U8, SampleType::U16, SampleType::F32] {
        for mode in [ColorMode::Grayscale, ColorMode::Rgb, ColorMode::Cmyk, ColorMode::Lab] {
            for alpha in [false, true] {
                let fmt = PixelFormat::new(mode, sample, alpha);
                let s = surface(fmt, B, noisy);
                for p in &params {
                    let out = run(&s, p);
                    assert_eq!(out.format(), fmt, "{}", p.label());
                    assert_eq!(out.read_region(B).len(), B.width() as usize * B.height() as usize * fmt.channels(), "{}", p.label());
                    // Tiling never changes the result (global filters prepare once; local ones use halos).
                    let area = output_area(p, s.content_bounds(), B, None);
                    let small = apply_tiled(&s, p, area, B, None, 7, None);
                    assert_eq!(small.read_region(B), out.read_region(B), "{} at {sample:?}/{mode:?}/alpha={alpha} depends on tiling", p.label());
                }
            }
        }
    }
}

#[test]
fn empty_and_degenerate_inputs_do_not_panic() {
    let empty = Surface::new(rgba_f32());
    for p in [
        FilterParams::PixelSort { low: 300.0, high: -5.0, vertical: true, reverse: true },
        FilterParams::Datamosh { block: 0, amount: 2.0, drift: u32::MAX, aberration: 1e9, seed: u32::MAX },
        FilterParams::RetroConsole { size: 0, levels: 0, dither: true },
        FilterParams::FilmNegative { mode: NegativeMode::Color, clip: f32::NAN },
        FilterParams::BeamDither {
            spec: BeamDitherSpec { scale: 0, levels: 0, beam_pitch: 0, blur: 1e9, sharpen: -1.0, noise: 1e9, ..BeamDitherSpec::default() },
        },
        FilterParams::ChromaDenoise { strength: f32::INFINITY },
        FilterParams::Sepia { amount: f32::NEG_INFINITY },
    ] {
        let _ = apply(&empty, &p, Rect::EMPTY, Rect::EMPTY, None);
        let _ = apply(&empty, &p, B, B, None);
        let one = surface(rgba_f32(), Rect::new(3, 3, 4, 4), noisy);
        let out = run(&one, &p);
        assert_eq!(out.read_region(Rect::new(3, 3, 4, 4)).len(), 4);
    }
}
