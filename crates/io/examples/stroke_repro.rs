//! Generate original, deterministic PSD artwork reproducing oblique group stroke artifacts.
//! `cargo run -p photocraft-io --example stroke_repro -- /tmp/stroke-repro`
//! Then resize master.psd to 588² and replace its Bar smart object's contents with bar.psb.
//! The printed layer ids can be used with the CLI's layer.showOnly / replaceContents commands.

#[path = "../../compose/tests/support/stroke_fixture.rs"]
mod stroke_fixture;

use std::{error::Error, path::PathBuf, sync::Arc};

use photocraft_color::SampleType;
use photocraft_doc::{Document, Layer, LayerContent, SmartSource};
use photocraft_io::{ExportOptions, export};

fn main() -> Result<(), Box<dyn Error>> {
    let path = PathBuf::from(std::env::args_os().nth(1).ok_or("provide an output directory")?);
    std::fs::create_dir_all(&path)?;
    let mut master = stroke_fixture::fixture(1, SampleType::U8, true);
    let native = stroke_fixture::fixture(4, SampleType::U8, false);
    let pixels = native.layers.first().and_then(Layer::children).and_then(|l| l.first()).and_then(Layer::surface).ok_or("missing native pixels")?;
    let mut source = Document::new("Original blue bar", native.size, native.mode, native.depth);
    source.layers.push(Layer::new("Bar pixels", LayerContent::Raster(pixels.clone())));
    let opts = ExportOptions::default();
    let bytes = export(&source, "bar.psb", &opts)?.bytes;
    std::fs::write(path.join("bar.psb"), &bytes)?;
    let group = master.layers.first_mut().ok_or("missing group")?;
    let group_id = group.id;
    let child = group.children_mut().and_then(|l| l.first_mut()).ok_or("missing smart object")?;
    if let LayerContent::Smart(sm) = &mut child.content {
        sm.source = SmartSource::Embedded { file_name: "bar.psb".into(), bytes: Arc::new(bytes) };
        sm.transform.m = [0.25, 0.0, 0.0, 0.25, 0.0, 0.0];
    }
    println!("group={}, smart={}", group_id.0, child.id.0);
    std::fs::write(path.join("master.psd"), export(&master, "master.psd", &opts)?.bytes)?;
    std::fs::write(path.join("master.png"), export(&master, "master.png", &opts)?.bytes)?;
    std::fs::write(path.join("direct-588.png"), export(&stroke_fixture::fixture(3, SampleType::U8, true), "direct.png", &opts)?.bytes)?;
    Ok(())
}
