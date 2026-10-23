//! Unsupported balance settings must not disappear in destructive adjustment commands.
use photocraft_engine::Session;
use serde_json::json;

#[test]
fn destructive_vibrance_refuses_balance_before_document_or_history_changes() {
    for depth in [8, 16, 32] {
        for target in [json!(null), json!("mask"), json!("quickMask"), json!({"channel": 0})] {
            for values in [json!({"temperature": 30}), json!({"tint": -20}), json!({"temperature": 10, "vibrance": 50, "saturation": -10})] {
                let mut s = Session::new();
                s.execute("file.new", json!({"width": 8, "height": 8, "depth": depth})).unwrap();
                s.execute("layer.new.layer", json!({})).unwrap();
                s.edit("paint", |doc, active| {
                    doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap().fill_rect(photocraft_geom::Rect::new(0, 0, 8, 8), &[0.3, 0.6, 0.1, 1.0]);
                    Ok(())
                })
                .unwrap();
                if target == json!("mask") {
                    s.execute("layer.layerMask.revealAll", json!({})).unwrap();
                } else if target == json!("quickMask") {
                    s.execute("select.editInQuickMaskMode", json!({"on": true})).unwrap();
                } else if target.is_object() {
                    s.execute("channel.new", json!({"name": "Alpha"})).unwrap();
                }
                let before = photocraft_format::save_to_bytes(&s.active().unwrap().doc, &Default::default()).unwrap();
                let undo = s.active().unwrap().history.undo_label().map(str::to_owned);
                let mut params = values;
                if !target.is_null() {
                    params["target"] = target.clone();
                }
                let error = s.execute("image.adjustments.vibrance", params).expect_err("unsupported destructive balance");
                assert!(error.to_string().contains("temperature/tint rendering is unsupported"), "{error}");
                assert_eq!(photocraft_format::save_to_bytes(&s.active().unwrap().doc, &Default::default()).unwrap(), before);
                assert_eq!(s.active().unwrap().history.undo_label(), undo.as_deref());
            }
        }
    }
}

#[test]
fn nondestructive_vibrance_preserves_balance_and_zero_balance_still_bakes() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.edit("paint", |doc, active| {
        doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap().fill_rect(photocraft_geom::Rect::new(0, 0, 8, 8), &[0.3, 0.6, 0.1, 1.0]);
        Ok(())
    })
    .unwrap();
    let before = photocraft_format::save_to_bytes(&s.active().unwrap().doc, &Default::default()).unwrap();
    s.execute("image.adjustments.vibrance", json!({"vibrance": 20, "temperature": 0, "tint": 0})).unwrap();
    assert_ne!(photocraft_format::save_to_bytes(&s.active().unwrap().doc, &Default::default()).unwrap(), before);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(photocraft_format::save_to_bytes(&s.active().unwrap().doc, &Default::default()).unwrap(), before);
    s.execute("layer.newAdjustmentLayer.vibrance", json!({"temperature": 25, "tint": -15})).unwrap();
    assert!(s.active().unwrap().doc.layers.iter().any(|l| matches!(&l.content,
        photocraft_doc::LayerContent::Adjustment(photocraft_doc::Adjustment::Vibrance {temperature, tint, ..}) if *temperature == 25.0 && *tint == -15.0)));
}

#[test]
fn smart_filter_preserves_balance_without_destructive_baking() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.edit("paint", |doc, active| {
        doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap().fill_rect(photocraft_geom::Rect::new(0, 0, 8, 8), &[0.3, 0.6, 0.1, 1.0]);
        Ok(())
    })
    .unwrap();
    let id = s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap()["layer"].as_u64().unwrap();
    s.execute("image.adjustments.vibrance", json!({"temperature": 25, "tint": -15, "vibrance": 30})).unwrap();
    let layer = s.active().unwrap().doc.layer(photocraft_doc::LayerId(id)).unwrap();
    let photocraft_doc::LayerContent::Smart(smart) = &layer.content else { panic!("smart object was baked") };
    assert_eq!(smart.smart_filters.len(), 1);
    let params = &smart.smart_filters[0].params;
    assert_eq!(params["temperature"], 25);
    assert_eq!(params["tint"], -15);
    assert_eq!(params["vibrance"], 30);
}
