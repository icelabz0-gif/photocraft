//! PSD/PSB Vibrance method flags PhotoCraft cannot render stay opaque and editable only by replacement.

use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::{Adjustment, Document, Layer, LayerContent};
use photocraft_geom::Size;
use photocraft_io::*;
use photocraft_psd::descriptor::{Descriptor, Value, VersionedDescriptor};
use photocraft_psd::testgen;
use photocraft_psd::{ColorMode as PsdColorMode, Compression, LayerFlags, LayerRecord, PsdFile, TaggedBlock, Version};

fn vibrance_block(method: Option<Value>) -> Vec<u8> {
    let mut desc = Descriptor::new("null").with("vibrance", Value::Integer(28)).with("Strt", Value::Integer(-7));
    if let Some(method) = method {
        desc = desc.with("useLegacy", method);
    }
    VersionedDescriptor::new(desc).to_bytes()
}

fn adjustment_record(raw: Vec<u8>) -> LayerRecord {
    let mut flags = LayerFlags::default();
    flags.set_pixel_data_irrelevant(true);
    LayerRecord {
        name: b"Modern Vibrance".to_vec(),
        flags,
        blocks: vec![TaggedBlock::new(*b"vibA", raw), TaggedBlock::unicode_name("Modern Vibrance"), TaggedBlock::layer_id(77)],
        ..Default::default()
    }
}

fn fixture(version: Version, depth: u16, method: Option<Value>) -> Vec<u8> {
    let mut file = testgen::layered(version, PsdColorMode::Rgb, depth, Compression::Raw);
    file.layers_mut().push(adjustment_record(vibrance_block(method)));
    file.to_bytes().expect("synthetic PSD/PSB fixture")
}

fn vib_block(file: &PsdFile) -> &[u8] {
    file.layers()
        .iter()
        .find(|layer| layer.name() == "Modern Vibrance")
        .and_then(|layer| layer.block(b"vibA"))
        .map(|block| block.data.as_slice())
        .expect("fixture Vibrance block")
}

fn named_layer(doc: &Document) -> &Layer {
    doc.layers.iter().find(|layer| layer.name == "Modern Vibrance").expect("imported adjustment layer")
}

#[test]
fn modern_vibrance_method_is_warned_and_preserved_through_psd_psb_and_pcraft() {
    for version in [Version::Psd, Version::Psb] {
        let path = if version == Version::Psd { "modern.psd" } else { "modern.psb" };
        for depth in [8, 16, 32] {
            let fixture_bytes = fixture(version, depth, Some(Value::Boolean(false)));
            // Read the serialized source before comparing; the PSD block writer owns alignment.
            let source = PsdFile::from_bytes(&fixture_bytes).expect("parse serialized fixture");
            let expected_raw = vib_block(&source).to_vec();
            let imported = import(path, &fixture_bytes).expect("import modern Vibrance");
            let layer = named_layer(&imported.document);
            assert!(
                matches!(&layer.content, LayerContent::Adjustment(Adjustment::Unsupported { psd_key, raw }) if psd_key == "vibA" && *raw == expected_raw),
                "{path} {depth}-bit: {:?}",
                layer.content
            );
            let warnings: Vec<&String> = imported.warnings.iter().filter(|warning| warning.contains("Modern Vibrance")).collect();
            assert_eq!(warnings.len(), 1, "{path} {depth}-bit: {:?}", imported.warnings);
            assert!(warnings[0].contains("modern Vibrance method") && warnings[0].contains("no effect"), "{}", warnings[0]);

            let pcraft = export(&imported.document, "kept.pcraft", &ExportOptions::default()).expect("save native document");
            let reopened = import("kept.pcraft", &pcraft.bytes).expect("reopen native document");
            assert!(matches!(
                &named_layer(&reopened.document).content,
                LayerContent::Adjustment(Adjustment::Unsupported { psd_key, raw }) if psd_key == "vibA" && *raw == expected_raw
            ));

            let saved = export(&reopened.document, path, &ExportOptions::default()).expect("save PSD/PSB");
            let saved_file = PsdFile::from_bytes(&saved.bytes).expect("parse saved PSD/PSB");
            assert_eq!(vib_block(&saved_file), expected_raw, "{path} {depth}-bit");
            let reopened_psd = import(path, &saved.bytes).expect("reopen saved PSD/PSB");
            assert!(matches!(
                &named_layer(&reopened_psd.document).content,
                LayerContent::Adjustment(Adjustment::Unsupported { psd_key, raw }) if psd_key == "vibA" && *raw == expected_raw
            ));
        }
    }
}

#[test]
fn legacy_vibrance_method_defaults_remain_supported_at_every_depth() {
    for version in [Version::Psd, Version::Psb] {
        let path = if version == Version::Psd { "legacy.psd" } else { "legacy.psb" };
        for depth in [8, 16, 32] {
            for method in [None, Some(Value::Boolean(true))] {
                let fixture_bytes = fixture(version, depth, method);
                let imported = import(path, &fixture_bytes).expect("import legacy Vibrance");
                assert!(
                    matches!(&named_layer(&imported.document).content, LayerContent::Adjustment(Adjustment::Vibrance { vibrance: 28.0, saturation: -7.0 })),
                    "{path} {depth}-bit: {:?}",
                    named_layer(&imported.document).content
                );
                assert!(imported.warnings.iter().all(|warning| !warning.contains("Modern Vibrance")), "{path} {depth}-bit: {:?}", imported.warnings);
            }
        }
    }
}

#[test]
fn unsupported_vibrance_keeps_its_raw_block_after_pcraft_save() {
    let raw = vibrance_block(Some(Value::Boolean(false)));
    let mut doc = Document::new("synthetic", Size::new(16, 16), ColorMode::Rgb, SampleType::U8);
    let mut layer = Layer::new("Modern Vibrance", LayerContent::Adjustment(Adjustment::Unsupported { psd_key: "vibA".into(), raw: raw.clone() }));
    layer.psd_blocks.push((*b"vibA", std::sync::Arc::new(raw.clone())));
    doc.layers.push(layer);
    let saved = export(&doc, "native.pcraft", &ExportOptions::default()).expect("save native document");
    let reopened = import("native.pcraft", &saved.bytes).expect("reopen native document");
    let layer = named_layer(&reopened.document);
    assert!(matches!(&layer.content, LayerContent::Adjustment(Adjustment::Unsupported { psd_key, raw: kept }) if psd_key == "vibA" && kept == &raw));
    assert!(layer.psd_blocks.iter().any(|(key, data)| key == b"vibA" && data.as_slice() == raw));
}
