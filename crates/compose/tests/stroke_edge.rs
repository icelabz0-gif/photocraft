//! A group's outside stroke must follow the subpixel edge of its smart object's cached pixels.

#[path = "support/stroke_fixture.rs"]
mod stroke_fixture;

use photocraft_color::SampleType;
use photocraft_compose::{render, render_tiled};
use photocraft_doc::LayerContent;

#[test]
fn stroke_retains_translucent_features_next_to_an_opaque_contour() {
    let mut d = stroke_fixture::fixture(1, SampleType::U8, false);
    let group = d.layers.first_mut().unwrap();
    let smart = group.children_mut().unwrap().first_mut().unwrap();
    let LayerContent::Smart(sm) = &mut smart.content else { panic!("fixture smart object") };
    // A separate translucent speck just outside the opaque bar's stroke. Reconstructing the
    // opaque contour must not erase its own outside stroke in the overlap between the two.
    sm.cache.as_mut().unwrap().write_pixel(78, 125, &[0.1, 0.5, 0.85, 0.25]);
    let image = render(&d, d.bounds());
    assert!(image.get(80, 124)[3] > 0.99, "translucent feature lost its stroke: {:?}", image.get(80, 124));
}

#[test]
fn stroke_keeps_resampled_smart_edges_smooth() {
    let mut d = stroke_fixture::fixture(3, SampleType::U8, false);
    let group = d.layers.first_mut().unwrap();
    let smart = group.children_mut().unwrap().first_mut().unwrap();
    let LayerContent::Smart(sm) = &mut smart.content else { panic!("fixture smart object") };
    let source = sm.cache.as_ref().unwrap().clone();
    let output = sm.cache.as_mut().unwrap();
    // Model the two-pixel antialias footprint of a resampled smart-object cache. This is input
    // generation only; the renderer must offset it without filtering the exported silhouette.
    let weights = [0.125, 0.75, 0.125];
    for y in 0..588 {
        for x in 0..588 {
            let mut alpha = 0.0;
            for (dy, wy) in (-1..=1).zip(weights) {
                for (dx, wx) in (-1..=1).zip(weights) {
                    alpha += source.pixel(x + dx, y + dy)[3] * wx * wy;
                }
            }
            output.write_pixel(x, y, &[0.1, 0.5, 0.85, alpha]);
        }
    }
    let image = render(&d, d.bounds());
    let mut worst = 0.0f32;
    for y in 0..588 {
        for x in 0..588 {
            let (dx, dy) = (f64::from(x) + 0.5 - 294.0, f64::from(y) + 0.5 - 294.0);
            if (0.8 * dx + 0.6 * dy).abs() < 135.0 {
                let expected = stroke_fixture::coverage(x, y, 3, 18.0);
                worst = worst.max((image.get(x as i32, y as i32)[3] - expected).abs());
            }
        }
    }
    assert!(worst < 0.15, "resampled edge coverage error {worst}");
}

#[test]
fn smart_group_outside_stroke_preserves_oblique_edge_coverage() {
    for depth in [SampleType::U8, SampleType::U16, SampleType::F32] {
        for scale in [1, 3, 4] {
            let d = stroke_fixture::fixture(scale, depth, false);
            let image = render(&d, d.bounds());
            let mut worst = 0.0f32;
            let mut count = 0;
            for y in 0..d.size.height {
                for x in 0..d.size.width {
                    let (dx, dy) = (f64::from(x) + 0.5 - 98.0 * f64::from(scale), f64::from(y) + 0.5 - 98.0 * f64::from(scale));
                    // Stay away from the rounded ends: the exact reference here is a parallel
                    // offset of the bar's long straight edges, not an expanded square corner.
                    if (0.8 * dx + 0.6 * dy).abs() > 45.0 * f64::from(scale) {
                        continue;
                    }
                    let expected = stroke_fixture::coverage(x, y, scale, 6.0 * f64::from(scale));
                    if expected > 0.01 && expected < 0.99 {
                        let actual = image.get(x as i32, y as i32);
                        worst = worst.max((actual[3] - expected).abs());
                        if actual[3] > 0.01 {
                            assert!(actual[..3].iter().all(|v| *v > 0.99), "outer rim must stay white: {actual:?}");
                        }
                        count += 1;
                    }
                }
            }
            assert!(count > 50);
            // Linear interpolation of pixel coverage and the renderer's linear rim ramp differ
            // slightly from exact pixel area. The old centre-seeded field errs by over 0.11.
            assert!(worst < 0.09, "{depth:?}, scale {scale}: outside stroke coverage error {worst}");
        }
    }
}

#[test]
fn smart_group_stroke_keeps_shadow_and_tiled_rendering() {
    let d = stroke_fixture::fixture(1, SampleType::U8, true);
    let image = render(&d, d.bounds());
    let without_shadow = render(&stroke_fixture::fixture(1, SampleType::U8, false), d.bounds());
    let shadow_pixels = image.px.iter().zip(&without_shadow.px).filter(|(p, q)| q[3] == 0.0 && p[3] > 0.01 && p[..3].iter().all(|v| *v < 0.01)).count();
    assert!(shadow_pixels > 100, "a soft black shadow must remain visible outside the white stroke");
    let tiled = render_tiled(&d, d.bounds(), 47);
    for (a, b) in image.px.iter().zip(&tiled.px) {
        assert!(a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1.0 / 255.0), "tile seam: {a:?} / {b:?}");
    }
}

#[test]
fn smart_group_stroke_keeps_rounded_corners() {
    for scale in [1, 3, 4] {
        let d = stroke_fixture::fixture(scale, SampleType::U8, false);
        let image = render(&d, d.bounds());
        let mut worst = 0.0f32;
        for y in 0..d.size.height {
            for x in 0..d.size.width {
                let expected = stroke_fixture::coverage(x, y, scale, 6.0 * f64::from(scale));
                worst = worst.max((image.get(x as i32, y as i32)[3] - expected).abs());
            }
        }
        // A raster corner only locates the original vertex to a fraction of a pixel. Keep
        // that uncertainty bounded; the old field misses whole rim pixels at scale 3.
        assert!(worst < 0.5, "scale {scale}: corner coverage error {worst}");
    }
}
