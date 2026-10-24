//! Retouch AI (Phase 1): one analysis pass, then one non-destructive layer per tool.
//!
//! `retouchAI.run` analyses the source layer (or the composite) once at a reduced size, derives
//! skin/face/subject masks, and builds every requested result as a raster layer above the source.
//! Each layer holds pixels only inside its affected bounding box and a sparse grayscale mask that
//! reads 0 everywhere outside it, so the effect never leaks past the analysed region.
//!
//! The engine deliberately reuses the app's existing detectors and pixels algorithms:
//! [`photocraft_algo::faces`], Color Range's skin tones, Select Subject, matting helpers,
//! [`photocraft_algo::inpaint`], [`photocraft_algo::retouch`] and the Lab Match Color statistics.
//! No new models, network calls or heavy dependencies are introduced. Analysis is capped at a
//! 1600 px long side; pixel work happens at full resolution but only in the affected bbox, in
//! bands, so a 24 MP document never needs several full-canvas RGBA copies.

use std::path::Path;

use photocraft_algo::faces;
use photocraft_algo::inpaint::{self, CompleteParams};
use photocraft_algo::matting;
use photocraft_algo::retouch::{ToneRange, dodge_burn};
use photocraft_algo::segment::{self, RgbImage, subject};
use photocraft_algo::selection;
use photocraft_algo::tone::{self, LabStats, MatchColor};
use photocraft_color::convert::{lab_to_srgb, srgb_to_lab};
use photocraft_color::{BlendMode, PixelFormat};
use photocraft_doc::{Document, Layer, LayerContent, LayerId, LayerMask};
use photocraft_geom::Rect;
use photocraft_raster::{Surface, from_rgba_into, to_rgba};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

const RUN: &str = "retouchAI.run";
const ANALYZE: &str = "retouchAI.analyze";
const FROM_FACE: &str = "layer.layerMask.fromFace";
const DETECT_FACES: &str = "layer.layerMask.detectFaces";
const MAX_TOOLS: usize = 14;
const ANALYSIS_SIDE: u32 = 1600;
const MASK_COVERAGE: f32 = 0.004;
/// Every result layer this command creates starts with this prefix.
const RESULT_PREFIX: &str = "Retouch • ";

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

// ---------------------------------------------------------------------------------------------
// Tools and parameters
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tool {
    Heal,
    DodgeBurn,
    SkinTone,
    Mattifier,
    SkinMask,
    PortraitVolumes,
    EyeBrilliance,
    EyeVessels,
    Teeth,
    CleanHair,
    FabricVolumes,
    DustClean,
    BackgroundClean,
    ColorMatch,
}

impl Tool {
    const ALL: [Tool; MAX_TOOLS] = [
        Tool::Heal,
        Tool::DodgeBurn,
        Tool::SkinTone,
        Tool::Mattifier,
        Tool::SkinMask,
        Tool::PortraitVolumes,
        Tool::EyeBrilliance,
        Tool::EyeVessels,
        Tool::Teeth,
        Tool::CleanHair,
        Tool::FabricVolumes,
        Tool::DustClean,
        Tool::BackgroundClean,
        Tool::ColorMatch,
    ];

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.id() == s)
    }

    fn id(self) -> &'static str {
        match self {
            Tool::Heal => "heal",
            Tool::DodgeBurn => "dodgeBurn",
            Tool::SkinTone => "skinTone",
            Tool::Mattifier => "mattifier",
            Tool::SkinMask => "skinMask",
            Tool::PortraitVolumes => "portraitVolumes",
            Tool::EyeBrilliance => "eyeBrilliance",
            Tool::EyeVessels => "eyeVessels",
            Tool::Teeth => "teeth",
            Tool::CleanHair => "cleanHair",
            Tool::FabricVolumes => "fabricVolumes",
            Tool::DustClean => "dustClean",
            Tool::BackgroundClean => "backgroundClean",
            Tool::ColorMatch => "colorMatch",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tool::Heal => "Heal",
            Tool::DodgeBurn => "Dodge & Burn",
            Tool::SkinTone => "Skin Tone",
            Tool::Mattifier => "Mattifier",
            Tool::SkinMask => "Skin Mask",
            Tool::PortraitVolumes => "Portrait Volumes",
            Tool::EyeBrilliance => "Eye Brilliance",
            Tool::EyeVessels => "Eye Vessels",
            Tool::Teeth => "Teeth",
            Tool::CleanHair => "Clean Hair",
            Tool::FabricVolumes => "Fabric Volumes",
            Tool::DustClean => "Dust Clean",
            Tool::BackgroundClean => "Background Clean",
            Tool::ColorMatch => "Color Match",
        }
    }

    fn layer_name(self) -> String {
        format!("{RESULT_PREFIX}{}", self.label())
    }
}

#[derive(Clone, Copy, Debug)]
struct RequestedTool {
    tool: Tool,
    amount: f32,
}

fn parse_tools(p: &Value) -> Result<Vec<RequestedTool>> {
    let list = p.get("tools").and_then(Value::as_array).ok_or_else(|| bad(RUN, "missing `tools` (1..14 tool objects)"))?;
    if list.is_empty() {
        return Err(bad(RUN, "`tools` is empty; pick at least one tool"));
    }
    if list.len() > MAX_TOOLS {
        return Err(bad(RUN, format!("`tools` has {} entries; at most {MAX_TOOLS} are allowed", list.len())));
    }
    let mut out = Vec::with_capacity(list.len());
    for (i, item) in list.iter().enumerate() {
        let o = item.as_object().ok_or_else(|| bad(RUN, format!("tools[{i}] must be an object")))?;
        let id = o.get("id").and_then(Value::as_str).ok_or_else(|| bad(RUN, format!("tools[{i}] needs a string `id`")))?;
        let tool = Tool::parse(id).ok_or_else(|| bad(RUN, format!("unknown tool `{id}`")))?;
        if out.iter().any(|t: &RequestedTool| t.tool == tool) {
            return Err(bad(RUN, format!("tool `{id}` is listed twice")));
        }
        let amount = match o.get("amount") {
            None => 100.0,
            Some(Value::Number(n)) => {
                let v = n.as_f64().ok_or_else(|| bad(RUN, format!("tools[{i}].amount must be a number in 0..100")))?;
                if !v.is_finite() {
                    return Err(bad(RUN, format!("tools[{i}].amount must be finite")));
                }
                v.clamp(0.0, 100.0) as f32
            }
            Some(_) => return Err(bad(RUN, format!("tools[{i}].amount must be a number in 0..100"))),
        };
        out.push(RequestedTool { tool, amount: amount / 100.0 });
    }
    Ok(out)
}

fn bool_param(p: &Value, key: &str, default: bool, cmd: &str) -> Result<bool> {
    match p.get(key) {
        None => Ok(default),
        Some(Value::Bool(v)) => Ok(*v),
        Some(_) => Err(bad(cmd, format!("`{key}` must be true or false"))),
    }
}

fn reference_path(p: &Value, needed: bool) -> Result<Option<String>> {
    match p.get("referencePath") {
        None => {
            if needed {
                Err(bad(RUN, "colorMatch needs `referencePath` (a local image file)"))
            } else {
                Ok(None)
            }
        }
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.clone())),
        Some(Value::String(_)) if needed => Err(bad(RUN, "`referencePath` is empty")),
        Some(Value::String(_)) => Ok(None),
        Some(_) => Err(bad(RUN, "`referencePath` must be a string path")),
    }
}

fn source_layer_id(s: &Session, p: &Value, cmd: &str) -> Result<LayerId> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    match p.get("layer") {
        Some(v) => {
            let n = v.as_u64().ok_or_else(|| bad(cmd, "`layer` must be a layer id"))?;
            let id = LayerId(n);
            if d.doc.layer(id).is_none() {
                return Err(EngineError::NoLayer(id));
            }
            Ok(id)
        }
        None => d.active_layer.ok_or_else(|| bad(cmd, "no active layer; choose a source layer")),
    }
}

// ---------------------------------------------------------------------------------------------
// Analysis masks
// ---------------------------------------------------------------------------------------------

/// A coverage field at analysis resolution, mapped onto a full-resolution rectangle.
#[derive(Clone, Debug)]
struct Field {
    rect: Rect,
    /// Tight full-resolution bounds of the coverage; `None` derives them from `data` on demand.
    content: Option<Rect>,
    w: usize,
    h: usize,
    data: Vec<f32>,
}

impl Field {
    fn empty(rect: Rect, w: usize, h: usize) -> Self {
        Self { rect, content: None, w, h, data: vec![0.0; w.saturating_mul(h)] }
    }

    fn filled(rect: Rect, w: usize, h: usize, data: Vec<f32>) -> Self {
        let n = w.saturating_mul(h);
        if data.len() == n { Self { rect, content: None, w, h, data } } else { Self::empty(rect, w, h) }
    }

    /// Records the affected region explicitly, so callers can allocate only this rectangle even
    /// when the field itself spans the whole analysis grid.
    fn with_content(mut self, content: Rect) -> Self {
        self.content = Some(content.intersect(&self.rect));
        self
    }

    /// The tight bbox a tool may write into. Fields without explicit bounds fall back to the
    /// coverage bbox, which is already tight for the analysis-generated masks.
    fn content_bounds(&self) -> Rect {
        self.content.unwrap_or_else(|| self.bbox(0.08)).intersect(&self.rect)
    }

    /// Bilinear coverage at a full-resolution pixel centre; 0 outside the field.
    fn at(&self, x: i32, y: i32) -> f32 {
        if self.w == 0 || self.h == 0 || self.rect.is_empty() || !self.rect.contains(x, y) {
            return 0.0;
        }
        let fx = ((x as f32 + 0.5 - self.rect.x0 as f32) * self.w as f32 / self.rect.width() as f32) - 0.5;
        let fy = ((y as f32 + 0.5 - self.rect.y0 as f32) * self.h as f32 / self.rect.height() as f32) - 0.5;
        let x0 = fx.floor();
        let y0 = fy.floor();
        let tx = (fx - x0).clamp(0.0, 1.0);
        let ty = (fy - y0).clamp(0.0, 1.0);
        let ix = |v: f32| v.clamp(0.0, self.w.saturating_sub(1) as f32) as usize;
        let iy = |v: f32| v.clamp(0.0, self.h.saturating_sub(1) as f32) as usize;
        let (x0, x1) = (ix(x0), ix(x0 + 1.0));
        let (y0, y1) = (iy(y0), iy(y0 + 1.0));
        let row0 = y0 * self.w;
        let row1 = y1 * self.w;
        let a = self.data[row0 + x0] * (1.0 - tx) + self.data[row0 + x1] * tx;
        let b = self.data[row1 + x0] * (1.0 - tx) + self.data[row1 + x1] * tx;
        (a * (1.0 - ty) + b * ty).clamp(0.0, 1.0)
    }

    /// Full-resolution bbox of samples above `threshold`, expanded by one pixel.
    fn bbox(&self, threshold: f32) -> Rect {
        sample_bbox(self.rect, self.w, self.h, &self.data, threshold)
    }

    fn mean(&self) -> f32 {
        if self.data.is_empty() {
            return 0.0;
        }
        self.data.iter().copied().sum::<f32>() / self.data.len() as f32
    }
}

/// Full-resolution bbox of the analysis samples above `threshold`, expanded by one pixel.
fn sample_bbox(rect: Rect, w: usize, h: usize, data: &[f32], threshold: f32) -> Rect {
    if w == 0 || h == 0 || data.len() != w.saturating_mul(h) {
        return Rect::EMPTY;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
    let mut any = false;
    for (i, v) in data.iter().enumerate() {
        if *v <= threshold {
            continue;
        }
        any = true;
        let (x, y) = (i % w, i / w);
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    if !any {
        return Rect::EMPTY;
    }
    let sx = rect.width() as f32 / w as f32;
    let sy = rect.height() as f32 / h as f32;
    let r = Rect::new(
        rect.x0 + (x0 as f32 * sx).floor() as i32,
        rect.y0 + (y0 as f32 * sy).floor() as i32,
        rect.x0 + ((x1 + 1) as f32 * sx).ceil() as i32,
        rect.y0 + ((y1 + 1) as f32 * sy).ceil() as i32,
    );
    r.inflate(1).intersect(&rect)
}

struct Analysis {
    canvas: Rect,
    aw: usize,
    ah: usize,
    faces: usize,
    eyes: usize,
    mouth: bool,
    skin: Field,
    skin_bbox: Rect,
    subject: Option<Field>,
    subject_bbox: Rect,
    eye: Field,
    vessels: Field,
    teeth: Field,
    shade: Field,
    luma: Vec<f32>,
    redness: Vec<f32>,
    redness_local: Vec<f32>,
    skin_lab: Option<[f32; 3]>,
    face_rects: Vec<Rect>,
}

fn analysis_size(canvas: Rect) -> (usize, usize) {
    let (w, h) = (canvas.width().max(1), canvas.height().max(1));
    let scale = (ANALYSIS_SIDE as f32 / w.max(h) as f32).min(1.0);
    let aw = ((w as f32 * scale).round() as usize).max(1).min(w as usize);
    let ah = ((h as f32 * scale).round() as usize).max(1).min(h as usize);
    (aw, ah)
}

/// Detector box in analysis-image pixels.
fn face_rect(f: &faces::Face) -> Rect {
    if !f.rect.iter().all(|v| v.is_finite()) {
        return Rect::EMPTY;
    }
    Rect::from_xywh(f.rect[0].floor() as i32, f.rect[1].floor() as i32, f.rect[2].max(0.0) as u32, f.rect[3].max(0.0) as u32)
}

fn rect_area(r: Rect) -> f64 {
    f64::from(r.width()) * f64::from(r.height())
}

fn rect_iou(a: Rect, b: Rect) -> f64 {
    let inter = rect_area(a.intersect(&b));
    let union = rect_area(a) + rect_area(b) - inter;
    if union > 0.0 { (inter / union).clamp(0.0, 1.0) } else { 0.0 }
}

/// Intersection over the smaller box: catches shifted copies whose areas differ (IoU alone can
/// stay low when one box extends well past the other).
fn rect_overlap(a: Rect, b: Rect) -> f64 {
    let inter = rect_area(a.intersect(&b));
    let smaller = rect_area(a).min(rect_area(b));
    if smaller > 0.0 { (inter / smaller).clamp(0.0, 1.0) } else { 0.0 }
}

/// Collapse the detector's duplicate boxes for very large/close faces. The detector sometimes
/// reports the same face several times with shifted boxes (IoU as low as ~0.32 on a close-up).
/// Only strong overlaps are merged, so separate faces in a group photo stay separate.
fn dedupe_faces(mut found: Vec<faces::Face>) -> Vec<faces::Face> {
    const DUPLICATE_IOU: f64 = 0.30;
    const DUPLICATE_OVERLAP: f64 = 0.35;
    const CONTAINED_CENTER_IOU: f64 = 0.15;
    const CONTAINED_AREA_RATIO: f64 = 0.55;
    found.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<faces::Face> = Vec::with_capacity(found.len());
    for f in found {
        let r = face_rect(&f);
        if r.is_empty() {
            continue;
        }
        let duplicate = kept.iter().any(|k| {
            let kr = face_rect(k);
            if kr.is_empty() {
                return false;
            }
            let iou = rect_iou(r, kr);
            if iou > DUPLICATE_IOU || rect_overlap(r, kr) > DUPLICATE_OVERLAP {
                return true;
            }
            // The detector also reports shifted copies with a comparable area: one centre sits
            // inside the other box. Require some overlap so two separate faces whose centres
            // merely fall near each other are not merged.
            let center = |b: Rect| (b.x0 + b.width() as i32 / 2, b.y0 + b.height() as i32 / 2);
            let (cx, cy) = center(r);
            let (kx, ky) = center(kr);
            let center_inside = kr.contains(cx, cy) || r.contains(kx, ky);
            if !center_inside {
                return false;
            }
            // A tight crop inside the full head (much smaller) or two comparable shifted boxes
            // are both duplicates; boxes side by side have neither centre inside the other.
            let small = rect_area(r).min(rect_area(kr));
            let large = rect_area(r).max(rect_area(kr));
            small < large * CONTAINED_AREA_RATIO || iou > CONTAINED_CENTER_IOU
        });
        if !duplicate {
            kept.push(f);
        }
    }
    kept
}

/// The skin a portrait tool should touch around one detected face: the face, neck and a little
/// upper chest. The Color Range classifier also matches beige walls and warm backgrounds, so
/// this spatial gate keeps a portrait's "skin" mask on the person instead of the whole frame.
fn face_skin_region(f: Rect, grid: Rect) -> Rect {
    let fw = f.width().max(1) as f32;
    let fh = f.height().max(1) as f32;
    Rect::new(
        (f.x0 as f32 - fw * 0.65).floor() as i32,
        (f.y0 as f32 - fh * 0.35).floor() as i32,
        (f.x1 as f32 + fw * 0.65).ceil() as i32,
        (f.y1 as f32 + fh * 0.80).ceil() as i32,
    )
    .intersect(&grid)
}

/// The tighter gate for `coverage:"face"`: the detected face box with a little padding for the
/// chin and forehead, but not the neck or upper chest.
fn face_only_region(f: Rect, grid: Rect) -> Rect {
    let fw = f.width().max(1) as f32;
    let fh = f.height().max(1) as f32;
    let pad_x = (fw * 0.08).max(2.0);
    let pad_y = (fh * 0.08).max(2.0);
    Rect::new(
        (f.x0 as f32 - pad_x).floor() as i32,
        (f.y0 as f32 - pad_y).floor() as i32,
        (f.x1 as f32 + pad_x).ceil() as i32,
        (f.y1 as f32 + pad_y).ceil() as i32,
    )
    .intersect(&grid)
}

/// Zero skin coverage outside the detected faces and return the tight full-resolution content
/// bbox, padded a little for the mask's soft edge. With no face detector (or no face), the raw
/// classifier remains the only signal, so its own content bbox is used.
fn clean_skin(data: &mut [f32], aw: usize, ah: usize, canvas: Rect, faces: &[Rect], subject_bbox: Rect, coverage: FaceCoverage) -> Rect {
    const SKIN_THRESHOLD: f32 = 0.12;
    if aw == 0 || ah == 0 || data.len() != aw.saturating_mul(ah) {
        return Rect::EMPTY;
    }
    let gate = if !faces.is_empty() {
        let grid = Rect::new(0, 0, aw as i32, ah as i32);
        let mut allowed = vec![false; data.len()];
        for f in faces {
            let r = match coverage {
                FaceCoverage::Face => face_only_region(*f, grid),
                FaceCoverage::FaceNeck => face_skin_region(*f, grid),
            };
            for y in r.y0.max(0)..r.y1.min(ah as i32) {
                let row = y as usize * aw;
                for x in r.x0.max(0)..r.x1.min(aw as i32) {
                    if let Some(v) = allowed.get_mut(row + x as usize) {
                        *v = true;
                    }
                }
            }
        }
        for (i, v) in data.iter_mut().enumerate() {
            if !allowed.get(i).copied().unwrap_or(false) {
                *v = 0.0;
            }
        }
        None
    } else if !subject_bbox.is_empty() {
        // No face: the subject bbox still keeps beige walls from turning the whole frame into
        // "skin". Convert it to analysis pixels before clearing outside it.
        let sx = aw as f32 / canvas.width().max(1) as f32;
        let sy = ah as f32 / canvas.height().max(1) as f32;
        Some(Rect::new(
            ((subject_bbox.x0 - canvas.x0) as f32 * sx).floor() as i32,
            ((subject_bbox.y0 - canvas.y0) as f32 * sy).floor() as i32,
            ((subject_bbox.x1 - canvas.x0) as f32 * sx).ceil() as i32,
            ((subject_bbox.y1 - canvas.y0) as f32 * sy).ceil() as i32,
        ))
    } else {
        None
    };
    if let Some(gate) = gate {
        for (i, v) in data.iter_mut().enumerate() {
            let x = (i % aw) as i32;
            let y = (i / aw) as i32;
            if !gate.contains(x, y) {
                *v = 0.0;
            }
        }
    }
    let bbox = sample_bbox(canvas, aw, ah, data, SKIN_THRESHOLD);
    if bbox.is_empty() {
        return bbox;
    }
    let pad = (canvas.width().min(canvas.height()) / 200).clamp(6, 24) as i32;
    bbox.inflate(pad).intersect(&canvas)
}

fn analysis_document(doc: &Document, source: Option<LayerId>, all_layers: bool) -> Document {
    let mut d = doc.clone();
    if all_layers {
        return d;
    }
    let Some(id) = source else { return d };
    let Some(path) = d.path_of(id) else { return d };
    // Hide every layer (including nested siblings), then make the source's ancestors visible
    // again. The source layer itself is visible so its pixels reach the reduced composite.
    let paths: Vec<_> = d.walk().into_iter().map(|(p, _, _)| p).collect();
    for p in paths {
        if let Some(l) = d.layer_at_mut(&p) {
            l.visible = false;
        }
    }
    for end in 1..=path.len() {
        if let Some(l) = d.layer_at_mut(&path[..end]) {
            l.visible = true;
        }
    }
    d
}

/// Select Subject on the analysis image itself. Its region is already in analysis pixels, so the
/// field is copied grid-for-grid; the field's rect maps it to full resolution when sampled.
fn field_from_subject(region: &photocraft_algo::selection::Region, canvas: Rect, aw: usize, ah: usize) -> Field {
    let mut data = vec![0.0; aw.saturating_mul(ah)];
    if region.bbox.is_empty() || aw == 0 || ah == 0 || canvas.is_empty() {
        return Field::filled(canvas, aw, ah, data);
    }
    for y in 0..ah {
        for x in 0..aw {
            data[y * aw + x] = region.at(x as i32, y as i32);
        }
    }
    let data = matting::box_mean(&data, aw, ah, 1);
    Field::filled(canvas, aw, ah, data)
}

fn luma_of(c: [f32; 4]) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

fn saturation_of(c: [f32; 4]) -> f32 {
    let (lo, hi) = (c[0].min(c[1]).min(c[2]), c[0].max(c[1]).max(c[2]));
    if hi <= 1e-6 { 0.0 } else { ((hi - lo) / hi).clamp(0.0, 1.0) }
}

fn ellipse_coverage(dx: f32, dy: f32) -> f32 {
    let d = (dx * dx + dy * dy).sqrt();
    ((1.15 - d) / 0.4).clamp(0.0, 1.0).min(1.0)
}

/// Paint an elliptical eye mask, and the red-vessel coverage inside its whites.
#[allow(clippy::too_many_arguments)]
fn paint_eye(px: &[[f32; 4]], aw: usize, ah: usize, cx: f32, cy: f32, rx: f32, ry: f32, eye: &mut [f32], vessels: &mut [f32]) {
    let x0 = (cx - rx * 1.25).floor().max(0.0) as usize;
    let x1 = ((cx + rx * 1.25).ceil() as i64).clamp(0, aw as i64) as usize;
    let y0 = (cy - ry * 1.25).floor().max(0.0) as usize;
    let y1 = ((cy + ry * 1.25).ceil() as i64).clamp(0, ah as i64) as usize;
    for y in y0..y1 {
        for x in x0..x1 {
            let dx = (x as f32 + 0.5 - cx) / rx.max(1.0);
            let dy = (y as f32 + 0.5 - cy) / ry.max(1.0);
            let cov = ellipse_coverage(dx, dy);
            if cov <= 0.0 {
                continue;
            }
            let i = y * aw + x;
            eye[i] = eye[i].max(cov);
            let c = px[i];
            if c[3] <= 0.0 {
                continue;
            }
            let l = luma_of(c);
            let white = ((l - 0.45) / 0.25).clamp(0.0, 1.0) * (1.0 - (saturation_of(c) / 0.30)).clamp(0.0, 1.0);
            let red = (c[0] - (c[1] + c[2]) * 0.5 - 0.004) / 0.075;
            let v = cov * white * red.clamp(0.0, 1.0);
            vessels[i] = vessels[i].max(v);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_teeth(px: &[[f32; 4]], aw: usize, ah: usize, cx: f32, cy: f32, rx: f32, ry: f32, teeth: &mut [f32]) {
    let x0 = (cx - rx * 1.2).floor().max(0.0) as usize;
    let x1 = ((cx + rx * 1.2).ceil() as i64).clamp(0, aw as i64) as usize;
    let y0 = (cy - ry * 1.2).floor().max(0.0) as usize;
    let y1 = ((cy + ry * 1.2).ceil() as i64).clamp(0, ah as i64) as usize;
    for y in y0..y1 {
        for x in x0..x1 {
            let dx = (x as f32 + 0.5 - cx) / rx.max(1.0);
            let dy = (y as f32 + 0.5 - cy) / ry.max(1.0);
            let cov = ellipse_coverage(dx, dy);
            if cov <= 0.0 {
                continue;
            }
            let c = px[y * aw + x];
            if c[3] <= 0.0 {
                continue;
            }
            let l = luma_of(c);
            let chroma = (c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])).clamp(0.0, 1.0);
            let white = ((l - 0.42) / 0.25).clamp(0.0, 1.0) * (1.0 - (chroma / 0.22)).clamp(0.0, 1.0);
            let i = y * aw + x;
            teeth[i] = teeth[i].max(cov * white);
        }
    }
}

fn analyze(doc: &Document, source: Option<LayerId>, all_layers: bool) -> Result<Analysis> {
    let canvas = doc.bounds();
    if canvas.is_empty() {
        return Err(EngineError::Other("the document has no pixels to analyse".into()));
    }
    let (aw, ah) = analysis_size(canvas);
    let adoc = analysis_document(doc, source, all_layers);
    let buf = photocraft_compose::render_reduced(&adoc, aw as u32, ah as u32);
    if buf.px.len() != aw.saturating_mul(ah) {
        return Err(EngineError::Other("the reduced analysis image has an unexpected size".into()));
    }
    let px = buf.px;
    let luma: Vec<f32> = px.iter().map(|p| luma_of(*p)).collect();
    let redness: Vec<f32> = px.iter().map(|p| p[0] - (p[1] + p[2]) * 0.5).collect();

    let found = if faces::AVAILABLE { dedupe_faces(faces::detect(&px, aw, ah, crate::face_cmds::MIN_SCORE).unwrap_or_default()) } else { Vec::new() };
    let face_rects: Vec<Rect> = found.iter().map(face_rect).filter(|r| !r.is_empty()).collect();

    // Subject: Select Subject on the analysis image. Running it at full resolution would defeat
    // the analysis cap and allocate a full-canvas mask just to throw it away.
    let rgb = RgbImage::from_rgba(&px, aw, ah);
    let sampler = segment::ImageSampler { img: &rgb, origin: (0, 0) };
    let region = subject::select_subject(&sampler, Rect::new(0, 0, aw as i32, ah as i32));
    let subject = region.as_ref().map(|r| field_from_subject(r, canvas, aw, ah));
    let subject_bbox = subject.as_ref().map_or(Rect::EMPTY, |f| f.bbox(0.35));
    let subject = subject.map(|f| f.with_content(subject_bbox));

    // Skin: the existing Color Range skin-tone classifier, lightly smoothed and gated to the
    // detected faces (or the subject when there is no face). It is the base of every complexion
    // tool and of `skinMask`.
    let raw_skin = selection::skin_tones(&px, 40.0);
    let mut skin_data = matting::box_mean(&raw_skin, aw, ah, 1);
    let skin_bbox = clean_skin(&mut skin_data, aw, ah, canvas, &face_rects, subject_bbox, FaceCoverage::FaceNeck);
    let skin = Field::filled(canvas, aw, ah, skin_data).with_content(skin_bbox);

    let mut eye_data = vec![0.0f32; aw * ah];
    let mut vessel_data = vec![0.0f32; aw * ah];
    let mut teeth_data = vec![0.0f32; aw * ah];
    let mut eyes = 0usize;
    let mut mouth = false;
    for f in &found {
        let (r0, r1, r3, r4) = (f.landmarks[0], f.landmarks[1], f.landmarks[3], f.landmarks[4]);
        let inter = ((r0[0] - r1[0]).powi(2) + (r0[1] - r1[1]).powi(2)).sqrt();
        if inter > 2.0 {
            let rx = (inter * 0.27).max(2.0);
            let ry = (inter * 0.18).max(1.5);
            for c in [r0, r1] {
                if c[0].is_finite() && c[1].is_finite() {
                    paint_eye(&px, aw, ah, c[0], c[1], rx, ry, &mut eye_data, &mut vessel_data);
                    eyes += 1;
                }
            }
        }
        let mouth_w = ((r3[0] - r4[0]).powi(2) + (r3[1] - r4[1]).powi(2)).sqrt();
        if mouth_w > 2.0 {
            mouth = true;
            let cx = (r3[0] + r4[0]) * 0.5;
            let cy = (r3[1] + r4[1]) * 0.5;
            paint_teeth(&px, aw, ah, cx, cy, mouth_w * 0.30, mouth_w * 0.14, &mut teeth_data);
        }
    }

    // Local tonal deviation (luma minus a small-radius mean) drives Dodge & Burn, portrait
    // volume, fabric folds and the conservative defect detectors. The radius covers blemish-
    // sized neighbourhoods even on small/thumbnail-sized documents.
    let radius = ((aw.min(ah) as f32 / 128.0).round() as usize).clamp(3, 8);
    let local = matting::box_mean(&luma, aw, ah, radius);
    let shade_data: Vec<f32> = luma.iter().zip(&local).map(|(l, m)| l - m).collect();
    let shade = Field::filled(canvas, aw, ah, shade_data);
    let redness_local = matting::box_mean(&redness, aw, ah, radius);

    // Mean Lab of the skin, the target of Skin Tone.
    let mut sum = [0.0f64; 3];
    let mut wsum = 0.0f64;
    for (i, p) in px.iter().enumerate() {
        let w = f64::from(skin.data.get(i).copied().unwrap_or(0.0));
        if w <= 0.05 || p[3] <= 0.0 {
            continue;
        }
        let lab = srgb_to_lab([p[0], p[1], p[2]]);
        for k in 0..3 {
            sum[k] += w * f64::from(lab[k]);
        }
        wsum += w;
    }
    let skin_lab = (wsum > 0.0).then(|| std::array::from_fn::<f32, 3, _>(|k| (sum[k] / wsum) as f32));

    Ok(Analysis {
        canvas,
        aw,
        ah,
        faces: found.len(),
        eyes,
        mouth,
        skin,
        skin_bbox,
        subject,
        subject_bbox,
        eye: Field::filled(canvas, aw, ah, eye_data),
        vessels: Field::filled(canvas, aw, ah, vessel_data),
        teeth: Field::filled(canvas, aw, ah, teeth_data),
        shade,
        luma,
        redness,
        redness_local,
        skin_lab,
        face_rects,
    })
}

fn rect_json(r: Rect) -> Value {
    json!([r.x0, r.y0, r.width(), r.height()])
}

fn analysis_json(a: &Analysis) -> Value {
    let mut regions = serde_json::Map::new();
    if !a.skin_bbox.is_empty() {
        regions.insert("skin".into(), rect_json(a.skin_bbox));
    }
    if !a.subject_bbox.is_empty() {
        regions.insert("subject".into(), rect_json(a.subject_bbox));
    }
    json!({
        "faces": a.faces,
        "skin": !a.skin_bbox.is_empty(),
        "subject": !a.subject_bbox.is_empty(),
        "eyes": a.eyes,
        "mouth": a.mouth,
        "regions": Value::Object(regions),
    })
}

fn suggestions(a: &Analysis) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    let mut add = |id: &'static str| {
        if !out.contains(&id) {
            out.push(id);
        }
    };
    if !a.skin_bbox.is_empty() {
        for id in ["heal", "dodgeBurn", "skinTone", "mattifier", "skinMask"] {
            add(id);
        }
    }
    if a.faces > 0 {
        for id in ["eyeBrilliance", "eyeVessels", "teeth"] {
            add(id);
        }
    }
    if a.subject.is_some() {
        for id in ["portraitVolumes", "fabricVolumes", "dustClean", "backgroundClean"] {
            add(id);
        }
    }
    if a.faces > 0 && a.subject.is_some() {
        add("cleanHair");
    }
    out
}

/// Connected components of a coverage field, capped by component size, count and total area.
fn component_rects(field: &Field, threshold: f32, max_component_px: usize, max_count: usize, max_total_px: usize) -> Vec<Rect> {
    let (w, h) = (field.w, field.h);
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let mut seen = vec![false; w * h];
    let mut candidates: Vec<(Rect, usize, f32)> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for start in 0..w * h {
        if seen[start] || field.data.get(start).copied().unwrap_or(0.0) <= threshold {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
        let mut area = 0usize;
        let mut peak = 0.0f32;
        while let Some(i) = stack.pop() {
            area += 1;
            let (x, y) = (i % w, i / w);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            peak = peak.max(field.data.get(i).copied().unwrap_or(0.0));
            let mut visit = |j: usize| {
                if !seen[j] && field.data.get(j).copied().unwrap_or(0.0) > threshold {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 {
                visit(i - 1);
            }
            if x + 1 < w {
                visit(i + 1);
            }
            if y > 0 {
                visit(i - w);
            }
            if y + 1 < h {
                visit(i + w);
            }
        }
        if area == 0 || area > max_component_px {
            continue;
        }
        let sx = field.rect.width() as f32 / w as f32;
        let sy = field.rect.height() as f32 / h as f32;
        let r = Rect::new(
            field.rect.x0 + (x0 as f32 * sx).floor() as i32,
            field.rect.y0 + (y0 as f32 * sy).floor() as i32,
            field.rect.x0 + ((x1 + 1) as f32 * sx).ceil() as i32,
            field.rect.y0 + ((y1 + 1) as f32 * sy).ceil() as i32,
        )
        .inflate(2)
        .intersect(&field.rect);
        candidates.push((r, area, peak));
    }
    candidates.sort_by(|a, b| b.2.total_cmp(&a.2));
    let mut out: Vec<Rect> = Vec::new();
    let mut total = 0usize;
    for (r, area, _) in candidates {
        if out.len() >= max_count || total.saturating_add(area) > max_total_px {
            break;
        }
        total += area;
        out.push(r);
    }
    // Merge overlapping boxes so no pixel is written twice.
    let mut merged = true;
    while merged {
        merged = false;
        'outer: for i in 0..out.len() {
            for j in (i + 1)..out.len() {
                if !out[i].intersect(&out[j]).is_empty() {
                    out[i] = out[i].union(&out[j]);
                    out.remove(j);
                    merged = true;
                    break 'outer;
                }
            }
        }
    }
    out
}

fn field_mask_surface(field: &Field, area: Rect, selection: Option<&Surface>) -> Surface {
    let mut bbox = field.content_bounds().intersect(&area);
    if let Some(sel) = selection {
        bbox = bbox.intersect(&sel.content_bounds());
    }
    let mut s = Surface::new(PixelFormat::GRAY8);
    if bbox.is_empty() {
        return s;
    }
    let sel = selection.map(|sel| selection::mask_from_surface(Some(sel), bbox));
    let mut data = Vec::with_capacity(bbox.width() as usize * bbox.height() as usize);
    for y in bbox.y0..bbox.y1 {
        for x in bbox.x0..bbox.x1 {
            let mut m = field.at(x, y);
            if let Some(sel) = &sel {
                m *= sel.get(((y - bbox.y0) * bbox.width() as i32 + (x - bbox.x0)) as usize).copied().unwrap_or(0.0);
            }
            data.push(m.clamp(0.0, 1.0));
        }
    }
    s.write_region(bbox, &data);
    s.prune();
    s
}

fn hide_mask(surface: Surface) -> LayerMask {
    // The mask reads 0 everywhere the tool did not write, so an effect cannot leak outside its
    // analysed region. `hide_all` also supplies the default density/feather/links.
    LayerMask { surface, ..LayerMask::hide_all() }
}

// ---------------------------------------------------------------------------------------------
// Layer builders
// ---------------------------------------------------------------------------------------------

struct Ctx<'a> {
    doc: &'a Document,
    source: &'a Layer,
    fmt: PixelFormat,
    analysis: &'a Analysis,
    selection: Option<&'a Surface>,
    selection_only: bool,
}

impl Ctx<'_> {
    /// The selection the command is allowed to use: only when `selectionOnly` is true.
    fn active_selection(&self) -> Option<&Surface> {
        if self.selection_only { self.selection } else { None }
    }

    fn source_bounds(&self) -> Rect {
        self.source.surface().map(|s| s.content_bounds()).unwrap_or(Rect::EMPTY).intersect(&self.doc.bounds())
    }
}

#[allow(clippy::large_enum_variant)]
enum Outcome {
    Layer(Layer),
    Skipped(String),
}

fn skipped(reason: impl Into<String>) -> Outcome {
    Outcome::Skipped(reason.into())
}

fn new_layer(name: &str, fmt: PixelFormat) -> Layer {
    Layer::raster(name, fmt)
}

fn native_from_rgba(fmt: &PixelFormat, px: &[[f32; 4]]) -> Vec<f32> {
    let ch = fmt.channels();
    let mut out = vec![0.0f32; px.len().saturating_mul(ch)];
    let mut tmp = [0.0f32; 4];
    for (i, p) in px.iter().enumerate() {
        let _ = from_rgba_into(fmt, *p, &mut tmp);
        out[i * ch..(i + 1) * ch].copy_from_slice(&tmp[..ch]);
    }
    out
}

/// Build a layer whose pixels are `f(source, coverage, strength, x, y)` inside the field bbox,
/// written in bands so a 24 MP mask never needs a full-canvas RGBA copy.
fn field_layer<F>(ctx: &Ctx<'_>, tool: Tool, field: &Field, amount: f32, mut f: F) -> Outcome
where
    F: FnMut([f32; 4], f32, f32, i32, i32) -> [f32; 4],
{
    let Some(src) = ctx.source.surface() else {
        return skipped("source layer has no pixels");
    };
    let sel = ctx.active_selection();
    let mut bbox = field.content_bounds().intersect(&ctx.source_bounds());
    if let Some(sel) = sel {
        bbox = bbox.intersect(&sel.content_bounds());
    }
    if bbox.is_empty() {
        return skipped(format!("no {} region found", tool.id()));
    }
    let mut layer = new_layer(&tool.layer_name(), ctx.fmt);
    let mut mask = Surface::new(PixelFormat::GRAY8);
    let band = 128i32;
    {
        let out = match crate::pixels_mut(&mut layer) {
            Ok(s) => s,
            Err(e) => return skipped(e.to_string()),
        };
        let mut y = bbox.y0;
        while y < bbox.y1 {
            let y1 = (y + band).min(bbox.y1);
            let r = Rect::new(bbox.x0, y, bbox.x1, y1);
            let native = src.read_region(r);
            let ch = ctx.fmt.channels();
            let n = r.width() as usize * r.height() as usize;
            let mut rgba: Vec<[f32; 4]> = Vec::with_capacity(n);
            for p in native.chunks_exact(ch) {
                rgba.push(to_rgba(&ctx.fmt, p));
            }
            let sel_vals = sel.map(|s| selection::mask_from_surface(Some(s), r));
            let mut out_rgba = Vec::with_capacity(n);
            let mut mask_vals = Vec::with_capacity(n);
            for (i, p) in rgba.iter().enumerate() {
                let x = r.x0 + (i % r.width() as usize) as i32;
                let py = r.y0 + (i / r.width() as usize) as i32;
                let mut m = field.at(x, py);
                if let Some(sv) = &sel_vals {
                    m *= sv.get(i).copied().unwrap_or(0.0);
                }
                m = m.clamp(0.0, 1.0);
                mask_vals.push(m);
                // Coverage lives in the mask; the slider sets the pixel strength once.
                out_rgba.push(if m > MASK_COVERAGE { f(*p, m, amount, x, py) } else { *p });
            }
            out.write_region(r, &native_from_rgba(&ctx.fmt, &out_rgba));
            mask.write_region(r, &mask_vals);
            y = y1;
        }
    }
    mask.prune();
    if mask.content_bounds().is_empty() {
        return skipped(format!("no {} effect found", tool.id()));
    }
    layer.mask = Some(hide_mask(mask));
    Outcome::Layer(layer)
}

fn skin_mask_layer(ctx: &Ctx<'_>) -> Outcome {
    let Some(src) = ctx.source.surface() else {
        return skipped("source layer has no pixels");
    };
    let bounds = ctx.source_bounds();
    if bounds.is_empty() {
        return skipped("source layer is empty");
    }
    let mask = field_mask_surface(&ctx.analysis.skin, ctx.doc.bounds(), ctx.active_selection());
    let copy_bounds = mask.content_bounds().intersect(&bounds);
    if copy_bounds.is_empty() {
        return skipped("no skin found");
    }
    let mut layer = new_layer(&Tool::SkinMask.layer_name(), ctx.fmt);
    {
        let out = match crate::pixels_mut(&mut layer) {
            Ok(s) => s,
            Err(e) => return skipped(e.to_string()),
        };
        // Duplicate only the skin bbox, in bands: the mask hides everything else, so copying
        // the whole layer would waste memory without changing a pixel.
        let band = 128i32;
        let mut y = copy_bounds.y0;
        while y < copy_bounds.y1 {
            let y1 = (y + band).min(copy_bounds.y1);
            let r = Rect::new(copy_bounds.x0, y, copy_bounds.x1, y1);
            out.write_region(r, &src.read_region(r));
            y = y1;
        }
    }
    layer.mask = Some(hide_mask(mask));
    Outcome::Layer(layer)
}

fn heal_field(a: &Analysis) -> Field {
    let n = a.aw.saturating_mul(a.ah);
    let mut data = vec![0.0f32; n];
    for (i, out) in data.iter_mut().enumerate() {
        let skin = a.skin.data.get(i).copied().unwrap_or(0.0);
        if skin <= 0.05 {
            continue;
        }
        let dark = ((-a.shade.data.get(i).copied().unwrap_or(0.0) - 0.030) / 0.10).clamp(0.0, 1.0);
        let red = ((a.redness.get(i).copied().unwrap_or(0.0) - a.redness_local.get(i).copied().unwrap_or(0.0) - 0.025) / 0.10).clamp(0.0, 1.0);
        let eye = a.eye.data.get(i).copied().unwrap_or(0.0);
        let teeth = a.teeth.data.get(i).copied().unwrap_or(0.0);
        *out = skin * dark.max(red * 0.8) * (1.0 - eye * 0.8) * (1.0 - teeth * 0.9);
    }
    Field::filled(a.canvas, a.aw, a.ah, data)
}

fn shine_field(a: &Analysis) -> Field {
    let n = a.aw.saturating_mul(a.ah);
    let mut data = vec![0.0f32; n];
    for (i, out) in data.iter_mut().enumerate() {
        let skin = a.skin.data.get(i).copied().unwrap_or(0.0);
        if skin <= 0.05 {
            continue;
        }
        let l = a.luma.get(i).copied().unwrap_or(0.0);
        let local = a.shade.data.get(i).copied().unwrap_or(0.0);
        let bright = ((l - 0.62) / 0.22).clamp(0.0, 1.0);
        let spec = ((local + 0.015) / 0.09).clamp(0.0, 1.0);
        *out = skin * bright * (0.35 + 0.65 * spec);
    }
    Field::filled(a.canvas, a.aw, a.ah, data)
}

fn dust_field(a: &Analysis) -> Field {
    let n = a.aw.saturating_mul(a.ah);
    let mut data = vec![0.0f32; n];
    for (i, out) in data.iter_mut().enumerate() {
        let skin = a.skin.data.get(i).copied().unwrap_or(0.0);
        let eye = a.eye.data.get(i).copied().unwrap_or(0.0);
        let teeth = a.teeth.data.get(i).copied().unwrap_or(0.0);
        let score = ((a.shade.data.get(i).copied().unwrap_or(0.0).abs() - 0.055) / 0.16).clamp(0.0, 1.0);
        *out = score * (1.0 - skin) * (1.0 - eye) * (1.0 - teeth);
    }
    Field::filled(a.canvas, a.aw, a.ah, data)
}

fn background_field(a: &Analysis) -> Option<Field> {
    let subject = a.subject.as_ref()?;
    if subject.mean() < 0.004 {
        return None;
    }
    let mut bg: Vec<f32> = subject.data.iter().map(|v| (1.0 - v).clamp(0.0, 1.0)).collect();
    bg = selection::contract(&bg, a.aw, a.ah, 2.0);
    let n = a.aw.saturating_mul(a.ah);
    let mut data = vec![0.0f32; n];
    for (i, out) in data.iter_mut().enumerate() {
        let score = ((a.shade.data.get(i).copied().unwrap_or(0.0).abs() - 0.04) / 0.13).clamp(0.0, 1.0);
        let skin = a.skin.data.get(i).copied().unwrap_or(0.0);
        *out = score * bg.get(i).copied().unwrap_or(0.0) * (1.0 - skin);
    }
    Some(Field::filled(a.canvas, a.aw, a.ah, data))
}

fn fabric_field(a: &Analysis) -> Option<Field> {
    let subject = a.subject.as_ref()?;
    let n = a.aw.saturating_mul(a.ah);
    let mut data = vec![0.0f32; n];
    for (i, out) in data.iter_mut().enumerate() {
        let subj = subject.data.get(i).copied().unwrap_or(0.0);
        let skin = a.skin.data.get(i).copied().unwrap_or(0.0);
        let eye = a.eye.data.get(i).copied().unwrap_or(0.0);
        let teeth = a.teeth.data.get(i).copied().unwrap_or(0.0);
        *out = subj * (1.0 - skin) * (1.0 - eye) * (1.0 - teeth);
    }
    Some(Field::filled(a.canvas, a.aw, a.ah, data).with_content(subject.content_bounds()))
}

fn hair_field(a: &Analysis) -> Field {
    let n = a.aw.saturating_mul(a.ah);
    let mut data = vec![0.0f32; n];
    let Some(subject) = a.subject.as_ref() else { return Field::filled(a.canvas, a.aw, a.ah, data) };
    if a.face_rects.is_empty() {
        return Field::filled(a.canvas, a.aw, a.ah, data);
    }
    let radius = a.face_rects.iter().map(|r| r.width() as f32 * 0.06).fold(2.0f32, f32::max).clamp(2.0, 8.0);
    let expanded = selection::expand(&subject.data, a.aw, a.ah, radius);
    let mut near = vec![false; n];
    for f in &a.face_rects {
        let x0 = (f.x0 as f32 - f.width() as f32 * 0.40).max(0.0) as usize;
        let x1 = ((f.x1 as f32 + f.width() as f32 * 0.40) as i64).clamp(0, a.aw as i64) as usize;
        let y0 = (f.y0 as f32 - f.height() as f32 * 0.60).max(0.0) as usize;
        let y1 = ((f.y1 as f32 + f.height() as f32 * 0.20) as i64).clamp(0, a.ah as i64) as usize;
        if x0 >= x1 || y0 >= y1 {
            continue;
        }
        for y in y0..y1 {
            near[y * a.aw + x0..y * a.aw + x1].fill(true);
        }
    }
    for i in 0..n {
        if !near[i] {
            continue;
        }
        let subj = subject.data.get(i).copied().unwrap_or(0.0);
        let ring = (expanded.get(i).copied().unwrap_or(0.0) - subj).clamp(0.0, 1.0);
        if ring <= 0.0 {
            continue;
        }
        let skin = a.skin.data.get(i).copied().unwrap_or(0.0);
        let edge = ((a.shade.data.get(i).copied().unwrap_or(0.0).abs() - 0.05) / 0.12).clamp(0.0, 1.0);
        data[i] = ring * (1.0 - skin) * edge;
    }
    Field::filled(a.canvas, a.aw, a.ah, data)
}

/// Inpaint the holes of a component's bbox from its surroundings, writing only that bbox.
fn inpaint_layer(ctx: &Ctx<'_>, tool: Tool, field: &Field, comps: &[Rect], amount: f32, threshold: f32, pad: i32) -> Outcome {
    let Some(src) = ctx.source.surface() else {
        return skipped("source layer has no pixels");
    };
    let doc_bounds = ctx.doc.bounds();
    let source_bounds = ctx.source_bounds();
    let sel = ctx.active_selection();
    let mut layer = new_layer(&tool.layer_name(), ctx.fmt);
    let params = CompleteParams { patch_radius: 3, em_iters: 4, min_em_iters: 2, pm_iters: 1, refine_extent: 96, ..Default::default() };
    let mut mask = Surface::new(PixelFormat::GRAY8);
    let mut wrote = false;
    {
        let out = match crate::pixels_mut(&mut layer) {
            Ok(s) => s,
            Err(e) => return skipped(e.to_string()),
        };
        for comp in comps {
            let bbox = comp.inflate(pad).intersect(&doc_bounds).intersect(&source_bounds);
            if bbox.is_empty() {
                continue;
            }
            let sel_vals = sel.map(|s| selection::mask_from_surface(Some(s), bbox));
            let (w, h) = (bbox.width() as usize, bbox.height() as usize);
            let mut field_vals = Vec::with_capacity(w.saturating_mul(h));
            let mut hole = Vec::with_capacity(w.saturating_mul(h));
            let mut hole_count = 0usize;
            for y in bbox.y0..bbox.y1 {
                for x in bbox.x0..bbox.x1 {
                    let i = ((y - bbox.y0) * bbox.width() as i32 + (x - bbox.x0)) as usize;
                    let mut m = field.at(x, y);
                    if let Some(sv) = &sel_vals {
                        m *= sv.get(i).copied().unwrap_or(0.0);
                    }
                    m = m.clamp(0.0, 1.0);
                    let is_hole = m >= threshold;
                    hole_count += usize::from(is_hole);
                    field_vals.push(m);
                    hole.push(is_hole);
                }
            }
            if hole_count == 0 || hole_count.saturating_mul(2) > w.saturating_mul(h) {
                continue;
            }
            let native = src.read_region(bbox);
            let ch = ctx.fmt.channels();
            let mut rgba: Vec<[f32; 4]> = native.chunks_exact(ch).map(|p| to_rgba(&ctx.fmt, p)).collect();
            let mut plane: Vec<f32> = Vec::with_capacity(w.saturating_mul(h).saturating_mul(4));
            for p in &rgba {
                plane.extend_from_slice(p);
            }
            let filled = inpaint::complete(w, h, 4, &plane, &hole, &params).unwrap_or_else(|| photocraft_algo::poisson::membrane_fill(w, h, 4, &plane, &hole));
            for (i, p) in rgba.iter_mut().enumerate() {
                if !hole.get(i).copied().unwrap_or(false) {
                    continue;
                }
                let k = amount.clamp(0.0, 1.0);
                for (c, before) in p.iter_mut().enumerate() {
                    let after = filled.get(i * 4 + c).copied().unwrap_or(*before);
                    *before += (after - *before) * k;
                }
            }
            // The mask is the defect itself; the padded ring is only context for the inpaint.
            let mask_vals: Vec<f32> = field_vals.iter().zip(&hole).map(|(m, h)| if *h { *m } else { 0.0 }).collect();
            out.write_region(bbox, &native_from_rgba(&ctx.fmt, &rgba));
            mask.write_region(bbox, &mask_vals);
            wrote = true;
        }
    }
    if !wrote {
        return skipped(format!("no {} defects found", tool.id()));
    }
    mask.prune();
    if mask.content_bounds().is_empty() {
        return skipped(format!("no {} defects found", tool.id()));
    }
    layer.mask = Some(hide_mask(mask));
    Outcome::Layer(layer)
}

fn heal_layer(ctx: &Ctx<'_>, amount: f32) -> Outcome {
    let field = heal_field(ctx.analysis);
    if field.bbox(0.22).is_empty() {
        return skipped("no blemishes found");
    }
    let max_total = (ctx.analysis.aw.saturating_mul(ctx.analysis.ah) as f32 * 0.02) as usize;
    let comps = component_rects(&field, 0.22, 600, 24, max_total);
    if comps.is_empty() {
        return skipped("no blemishes found");
    }
    inpaint_layer(ctx, Tool::Heal, &field, &comps, amount, 0.24, 6)
}

fn dust_layer(ctx: &Ctx<'_>, amount: f32) -> Outcome {
    let field = dust_field(ctx.analysis);
    if field.bbox(0.32).is_empty() {
        return skipped("no dust specks found");
    }
    let max_total = (ctx.analysis.aw.saturating_mul(ctx.analysis.ah) as f32 * 0.005) as usize;
    let comps = component_rects(&field, 0.32, 80, 24, max_total);
    if comps.is_empty() {
        return skipped("no dust specks found");
    }
    inpaint_layer(ctx, Tool::DustClean, &field, &comps, amount, 0.34, 4)
}

fn background_layer(ctx: &Ctx<'_>, amount: f32) -> Outcome {
    let Some(field) = background_field(ctx.analysis) else {
        return skipped("no subject found");
    };
    if field.bbox(0.26).is_empty() {
        return skipped("no background blemishes found");
    }
    let max_total = (ctx.analysis.aw.saturating_mul(ctx.analysis.ah) as f32 * 0.01) as usize;
    let comps = component_rects(&field, 0.26, 600, 16, max_total);
    if comps.is_empty() {
        return skipped("no background blemishes found");
    }
    inpaint_layer(ctx, Tool::BackgroundClean, &field, &comps, amount, 0.28, 8)
}

fn hair_layer(ctx: &Ctx<'_>, amount: f32) -> Outcome {
    if ctx.analysis.face_rects.is_empty() {
        return skipped("no face found");
    }
    if ctx.analysis.subject.is_none() {
        return skipped("no subject found");
    }
    let field = hair_field(ctx.analysis);
    if field.bbox(0.26).is_empty() {
        return skipped("no flyaway strands found");
    }
    let max_total = (ctx.analysis.aw.saturating_mul(ctx.analysis.ah) as f32 * 0.005) as usize;
    let comps = component_rects(&field, 0.26, 400, 20, max_total);
    if comps.is_empty() {
        return skipped("no flyaway strands found");
    }
    inpaint_layer(ctx, Tool::CleanHair, &field, &comps, amount, 0.28, 4)
}

fn portrait_volumes(ctx: &Ctx<'_>, amount: f32) -> Outcome {
    let Some(subject) = ctx.analysis.subject.as_ref() else {
        return skipped("no subject found");
    };
    if subject.mean() < 0.004 {
        return skipped("subject region is too small");
    }
    let shade = &ctx.analysis.shade;
    field_layer(ctx, Tool::PortraitVolumes, subject, amount, |src, _m, k, x, y| {
        let d = shade.at(x, y);
        let e = (d.abs() * 0.55).min(0.22) * k;
        if e <= 0.0 {
            return src;
        }
        let out = dodge_burn([src[0], src[1], src[2]], e, ToneRange::Midtones, d < 0.0, true);
        [out[0], out[1], out[2], src[3]]
    })
}

fn dodge_burn_layer(ctx: &Ctx<'_>, amount: f32) -> Outcome {
    let shade = &ctx.analysis.shade;
    field_layer(ctx, Tool::DodgeBurn, &ctx.analysis.skin, amount, |src, _m, k, x, y| {
        let d = shade.at(x, y);
        let e = (d.abs() * 1.6).min(0.35) * k;
        if e <= 0.0 {
            return src;
        }
        // Dark creases are dodged; relative highlights are burned.
        let out = dodge_burn([src[0], src[1], src[2]], e, ToneRange::Midtones, d > 0.0, true);
        [out[0], out[1], out[2], src[3]]
    })
}

fn skin_tone_layer(ctx: &Ctx<'_>, amount: f32) -> Outcome {
    let Some(mean) = ctx.analysis.skin_lab else {
        return skipped("no skin found");
    };
    field_layer(ctx, Tool::SkinTone, &ctx.analysis.skin, amount, |src, _m, k, _x, _y| {
        let c = [src[0], src[1], src[2]];
        let lab = srgb_to_lab(c);
        let kk = k.clamp(0.0, 1.0);
        // Pull a/b toward the skin's mean (less blotchy red/blue), keep most of the local
        // lightness and take a little yellow out.
        let l = lab[0] + (mean[0] - lab[0]) * 0.18 * kk;
        let a = lab[1] + (mean[1] - lab[1]) * 0.55 * kk;
        let b = lab[2] + (mean[2] - lab[2]) * 0.45 * kk - 1.1 * kk;
        let out = lab_to_srgb([l.clamp(0.0, 100.0), a, b]);
        [src[0] + (out[0] - src[0]) * 0.8 * kk, src[1] + (out[1] - src[1]) * 0.8 * kk, src[2] + (out[2] - src[2]) * 0.8 * kk, src[3]]
    })
}

fn mattifier_layer(ctx: &Ctx<'_>, amount: f32) -> Outcome {
    let field = shine_field(ctx.analysis);
    if field.bbox(0.18).is_empty() {
        return skipped("no shine found");
    }
    field_layer(ctx, Tool::Mattifier, &field, amount, |src, _m, k, _x, _y| {
        let l = luma_of(src);
        let l2 = l * (1.0 - 0.16 * k);
        let scale = if l > 1e-5 { l2 / l } else { 1.0 };
        let mut out = [src[0] * scale, src[1] * scale, src[2] * scale];
        let desat = 0.18 * k;
        for c in &mut out {
            *c += (l2 - *c) * desat;
        }
        [out[0].clamp(0.0, 1.0), out[1].clamp(0.0, 1.0), out[2].clamp(0.0, 1.0), src[3]]
    })
}

fn eye_brilliance_layer(ctx: &Ctx<'_>, amount: f32) -> Outcome {
    if ctx.analysis.eyes == 0 {
        return skipped("no eyes found");
    }
    field_layer(ctx, Tool::EyeBrilliance, &ctx.analysis.eye, amount, |src, _m, k, _x, _y| {
        let l = luma_of(src);
        let target = ((l - 0.45) * 1.20 + 0.46).clamp(0.0, 1.0);
        let scale = if l > 1e-5 { 1.0 + (target / l - 1.0) * k } else { 1.0 };
        let lift = if l > 0.82 { 0.035 * k } else { 0.0 };
        [(src[0] * scale + lift).clamp(0.0, 1.0), (src[1] * scale + lift).clamp(0.0, 1.0), (src[2] * scale + lift).clamp(0.0, 1.0), src[3]]
    })
}

fn eye_vessels_layer(ctx: &Ctx<'_>, amount: f32) -> Outcome {
    if ctx.analysis.eyes == 0 {
        return skipped("no eyes found");
    }
    let field = &ctx.analysis.vessels;
    if field.bbox(0.12).is_empty() {
        return skipped("no eye vessels found");
    }
    field_layer(ctx, Tool::EyeVessels, field, amount, |src, _m, k, _x, _y| {
        let l = luma_of(src);
        let mut out = [src[0] + (l - src[0]) * 0.68 * k, src[1] + (l - src[1]) * 0.68 * k, src[2] + (l - src[2]) * 0.68 * k];
        for c in &mut out {
            *c = (*c + 0.025 * k).clamp(0.0, 1.0);
        }
        [out[0], out[1], out[2], src[3]]
    })
}

fn teeth_layer(ctx: &Ctx<'_>, amount: f32) -> Outcome {
    if !ctx.analysis.mouth {
        return skipped("no mouth found");
    }
    let field = &ctx.analysis.teeth;
    if field.bbox(0.12).is_empty() {
        return skipped("no teeth found");
    }
    field_layer(ctx, Tool::Teeth, field, amount, |src, _m, k, _x, _y| {
        let lab = srgb_to_lab([src[0], src[1], src[2]]);
        let kk = k.clamp(0.0, 1.0);
        let out = lab_to_srgb([(lab[0] + 3.2 * kk).clamp(0.0, 100.0), lab[1] * (1.0 - 0.12 * kk), lab[2] - 12.0 * kk]);
        [src[0] + (out[0] - src[0]) * kk, src[1] + (out[1] - src[1]) * kk, src[2] + (out[2] - src[2]) * kk, src[3]]
    })
}

fn fabric_volumes(ctx: &Ctx<'_>, amount: f32) -> Outcome {
    if ctx.analysis.subject.is_none() {
        return skipped("no subject found");
    }
    let Some(field) = fabric_field(ctx.analysis) else {
        return skipped("no subject found");
    };
    if field.mean() < 0.004 {
        return skipped("clothing region is too small");
    }
    let shade = &ctx.analysis.shade;
    field_layer(ctx, Tool::FabricVolumes, &field, amount, |src, _m, k, x, y| {
        let d = shade.at(x, y);
        let e = (d.abs() * 0.85).min(0.20) * k;
        if e <= 0.0 {
            return src;
        }
        let out = dodge_burn([src[0], src[1], src[2]], e, ToneRange::Midtones, d > 0.0, true);
        [out[0], out[1], out[2], src[3]]
    })
}

fn color_match_layer(ctx: &Ctx<'_>, reference: &LabStats, amount: f32) -> Outcome {
    let Some(src) = ctx.source.surface() else {
        return skipped("source layer has no pixels");
    };
    let sel = ctx.active_selection();
    let mut bbox = ctx.source_bounds();
    if let Some(sel) = sel {
        bbox = bbox.intersect(&sel.content_bounds());
    }
    if bbox.is_empty() {
        return skipped("source layer is empty");
    }
    let stats_px = source_stats_px(ctx.doc, ctx.source, ctx.analysis.aw, ctx.analysis.ah);
    let Some(target) = tone::lab_stats(&stats_px, None) else {
        return skipped("source layer has no opaque pixels");
    };
    let mut layer = new_layer(&Tool::ColorMatch.layer_name(), ctx.fmt);
    let mut mask = Surface::new(PixelFormat::GRAY8);
    {
        let out = match crate::pixels_mut(&mut layer) {
            Ok(s) => s,
            Err(e) => return skipped(e.to_string()),
        };
        let band = 128i32;
        let mut y = bbox.y0;
        while y < bbox.y1 {
            let y1 = (y + band).min(bbox.y1);
            let r = Rect::new(bbox.x0, y, bbox.x1, y1);
            let n = r.width() as usize * r.height() as usize;
            let native = src.read_region(r);
            let ch = ctx.fmt.channels();
            let mut rgba: Vec<[f32; 4]> = native.chunks_exact(ch).map(|p| to_rgba(&ctx.fmt, p)).collect();
            let original = rgba.clone();
            tone::match_color(&mut rgba, &target, Some(reference), &MatchColor::default());
            let sel_vals = sel.map(|s| selection::mask_from_surface(Some(s), r));
            for (i, p) in rgba.iter_mut().enumerate() {
                let k = match &sel_vals {
                    Some(sv) if sv.get(i).copied().unwrap_or(0.0) > 0.0 => amount,
                    Some(_) => 0.0,
                    None => amount,
                }
                .clamp(0.0, 1.0);
                for c in 0..4 {
                    let before = original.get(i).map_or(0.0, |q| q[c]);
                    p[c] = before + (p[c] - before) * k;
                }
            }
            let mask_vals = match &sel_vals {
                Some(sv) => sv.clone(),
                None => vec![1.0; n],
            };
            out.write_region(r, &native_from_rgba(&ctx.fmt, &rgba));
            mask.write_region(r, &mask_vals);
            y = y1;
        }
    }
    mask.prune();
    if mask.content_bounds().is_empty() {
        return skipped("source layer is empty");
    }
    layer.mask = Some(hide_mask(mask));
    Outcome::Layer(layer)
}

/// Reduced pixels of the source layer alone, for Match Color target statistics.
fn source_stats_px(doc: &Document, source: &Layer, aw: usize, ah: usize) -> Vec<[f32; 4]> {
    let mut tmp = Document::new("retouch-ai-stats", doc.size, doc.mode, doc.depth);
    tmp.icc_profile = doc.icc_profile.clone();
    let mut l = source.clone();
    l.visible = true;
    l.opacity = 1.0;
    l.fill_opacity = 1.0;
    l.blend = BlendMode::Normal;
    l.clipped = false;
    l.mask = None;
    l.vector_mask = None;
    tmp.layers.push(l);
    photocraft_compose::render_reduced(&tmp, aw as u32, ah as u32).px
}

fn load_reference_stats(path: &str) -> Result<LabStats> {
    let bytes = crate::file_cmds::read_file(path).map_err(|e| bad(RUN, format!("could not read `referencePath` {path}: {e}")))?;
    let name = Path::new(path).file_name().and_then(|s| s.to_str()).unwrap_or(path);
    let imported = photocraft_io::import(name, &bytes).map_err(|e| bad(RUN, format!("could not decode reference image `{path}`: {e}")))?;
    let doc = imported.document;
    let canvas = doc.bounds();
    if canvas.is_empty() {
        return Err(bad(RUN, "the reference image is empty"));
    }
    let (w, h) = analysis_size(canvas);
    let px = photocraft_compose::render_reduced(&doc, w as u32, h as u32).px;
    tone::lab_stats(&px, None).ok_or_else(|| bad(RUN, "the reference image has no opaque pixels"))
}

fn build_tool(ctx: &Ctx<'_>, tool: Tool, amount: f32, reference: Option<&LabStats>) -> Outcome {
    match tool {
        Tool::Heal => heal_layer(ctx, amount),
        Tool::DodgeBurn => dodge_burn_layer(ctx, amount),
        Tool::SkinTone => skin_tone_layer(ctx, amount),
        Tool::Mattifier => mattifier_layer(ctx, amount),
        Tool::SkinMask => skin_mask_layer(ctx),
        Tool::PortraitVolumes => portrait_volumes(ctx, amount),
        Tool::EyeBrilliance => eye_brilliance_layer(ctx, amount),
        Tool::EyeVessels => eye_vessels_layer(ctx, amount),
        Tool::Teeth => teeth_layer(ctx, amount),
        Tool::CleanHair => hair_layer(ctx, amount),
        Tool::FabricVolumes => fabric_volumes(ctx, amount),
        Tool::DustClean => dust_layer(ctx, amount),
        Tool::BackgroundClean => background_layer(ctx, amount),
        Tool::ColorMatch => match reference {
            Some(stats) => color_match_layer(ctx, stats, amount),
            None => skipped("colorMatch needs referencePath"),
        },
    }
}

// ---------------------------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------------------------

fn run_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let requested = parse_tools(p)?;
    let all_layers = bool_param(p, "sampleAllLayers", true, RUN)?;
    let selection_only = bool_param(p, "selectionOnly", false, RUN)?;
    let needs_reference = requested.iter().any(|t| t.tool == Tool::ColorMatch && t.amount > 0.0);
    // A missing reference is not a command error: `build_tool` reports colorMatch as skipped so
    // the other tools in a combo still run. A path that is present but unreadable still errors.
    let reference_path = reference_path(p, false)?;
    let reference = match reference_path.as_deref() {
        Some(path) if needs_reference => Some(load_reference_stats(path)?),
        _ => None,
    };

    let d = s.active().ok_or(EngineError::NoDocument)?;
    let source_id = source_layer_id(s, p, RUN)?;
    let source = d.doc.layer(source_id).ok_or(EngineError::NoLayer(source_id))?;
    if source.surface().is_none() {
        return Err(EngineError::Other(format!("layer \"{}\" has no pixels to retouch", source.name)));
    }
    let analysis = analyze(&d.doc, Some(source_id), all_layers)?;
    let fmt = d.doc.pixel_format();
    let ctx = Ctx { doc: &d.doc, source, fmt, analysis: &analysis, selection: d.doc.selection.as_ref(), selection_only };

    let (built, skipped_tools) = {
        let mut built: Vec<(Tool, Layer)> = Vec::new();
        let mut skipped_tools: Vec<(Tool, String)> = Vec::new();
        for req in &requested {
            if req.amount <= 0.0 {
                skipped_tools.push((req.tool, "amount is 0".to_string()));
                continue;
            }
            match build_tool(&ctx, req.tool, req.amount, reference.as_ref()) {
                Outcome::Layer(layer) => built.push((req.tool, layer)),
                Outcome::Skipped(reason) => skipped_tools.push((req.tool, reason)),
            }
        }
        (built, skipped_tools)
    };

    let mut layer_json = Vec::new();
    let mut skipped_json: Vec<Value> = skipped_tools.iter().map(|(t, r)| json!({"tool": t.id(), "reason": r})).collect();
    if !built.is_empty() {
        let mut ids = Vec::with_capacity(built.len());
        s.edit("Retouch AI", |doc, active| {
            // Put results at the top of the root stack. Inserting directly above the source can
            // hide every result under a duplicate background or another full-canvas layer; a
            // retouch panel's new layers should be visible immediately. Insert in reverse so the
            // panel's tool order stays the same from top to bottom.
            for (tool, layer) in built.into_iter().rev() {
                let id = doc.insert_above(None, layer);
                ids.push((tool, id));
            }
            ids.reverse();
            // Keep the original source selected: the result layers are sparse, so using one as
            // the source of a second run would clip its effects to the first result's bbox.
            *active = Some(source_id);
            Ok(())
        })?;
        for (tool, id) in ids {
            layer_json.push(json!({
                "tool": tool.id(),
                "name": tool.layer_name(),
                "layer": id.0,
                "blend": "normal",
                "opacity": 1.0,
            }));
        }
    }
    // Keep the result order matching the request, even though layers are inserted bottom-up.
    let order = |id: &str| requested.iter().position(|r| r.tool.id() == id).unwrap_or(usize::MAX);
    layer_json.sort_by_key(|v| order(v.get("tool").and_then(Value::as_str).unwrap_or("")));
    skipped_json.sort_by_key(|v| order(v.get("tool").and_then(Value::as_str).unwrap_or("")));

    Ok(json!({
        "layers": layer_json,
        "analysis": analysis_json(&analysis),
        "skipped": skipped_json,
    }))
}

fn analyze_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let all_layers = bool_param(p, "sampleAllLayers", true, ANALYZE)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let source = match p.get("layer") {
        Some(v) => {
            let n = v.as_u64().ok_or_else(|| bad(ANALYZE, "`layer` must be a layer id"))?;
            let id = LayerId(n);
            if d.doc.layer(id).is_none() {
                return Err(EngineError::NoLayer(id));
            }
            Some(id)
        }
        None => d.active_layer,
    };
    let analysis = analyze(&d.doc, source, all_layers)?;
    let mut out = analysis_json(&analysis);
    out["suggestions"] = json!(suggestions(&analysis));
    Ok(out)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FaceChoice {
    All,
    Main,
    /// 1-based order in the reading order (left to right, then top to bottom).
    Index(usize),
}

impl FaceChoice {
    fn parse(p: &Value) -> Result<Self> {
        let bad_faces = || bad(FROM_FACE, "`faces` must be \"all\", \"main\" or a 1-based face number");
        match p.get("faces") {
            None => Ok(Self::All),
            Some(Value::String(s)) if s == "all" => Ok(Self::All),
            Some(Value::String(s)) if s == "main" => Ok(Self::Main),
            Some(Value::Number(n)) => n.as_u64().filter(|n| *n >= 1).map(|n| Self::Index(n as usize)).ok_or_else(bad_faces),
            Some(Value::String(s)) => s.parse::<usize>().ok().filter(|n| *n >= 1).map(Self::Index).ok_or_else(bad_faces),
            Some(_) => Err(bad_faces()),
        }
    }

    fn id(self) -> String {
        match self {
            Self::All => "all".into(),
            Self::Main => "main".into(),
            Self::Index(n) => n.to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FaceCoverage {
    /// The face box only (a small padding for the chin/forehead).
    Face,
    /// Face, neck and a little upper chest (the Retouch AI portrait gate).
    FaceNeck,
}

impl FaceCoverage {
    fn parse(p: &Value) -> Result<Self> {
        match p.get("coverage") {
            None => Ok(Self::FaceNeck),
            Some(Value::String(s)) if s == "faceNeck" => Ok(Self::FaceNeck),
            Some(Value::String(s)) if s == "face" => Ok(Self::Face),
            Some(_) => Err(bad(FROM_FACE, "`coverage` must be \"face\" or \"faceNeck\"")),
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Face => "face",
            Self::FaceNeck => "faceNeck",
        }
    }
}

/// The document with one layer hidden: the visible image without the mask target's own colour
/// effect. Hidden siblings and groups keep their state, so the analysis sees exactly what the
/// user sees apart from that layer.
fn without_layer(doc: &Document, id: LayerId) -> Document {
    let mut d = doc.clone();
    if let Some(path) = d.path_of(id)
        && let Some(l) = d.layer_at_mut(&path)
    {
        l.visible = false;
    }
    d
}

/// Face boxes in analysis pixels: the shared detector plus the same duplicate collapsing the
/// Retouch AI tools use. `override_rects` is the deterministic seam for tests (the detector needs
/// a real face image, which synthetic command tests do not have).
fn face_rects_at(px: &[[f32; 4]], aw: usize, ah: usize, override_rects: Option<&[Rect]>) -> Vec<Rect> {
    let grid = Rect::new(0, 0, aw as i32, ah as i32);
    let rects: Vec<Rect> = match override_rects {
        Some(rects) => rects.to_vec(),
        None if faces::AVAILABLE => dedupe_faces(faces::detect(px, aw, ah, crate::face_cmds::MIN_SCORE).unwrap_or_default()).iter().map(face_rect).collect(),
        None => Vec::new(),
    };
    let mut rects: Vec<Rect> = rects.into_iter().map(|r| r.intersect(&grid)).filter(|r| !r.is_empty()).collect();
    // Stable labels for the face picker: left to right, then top to bottom.
    rects.sort_by_key(|r| (r.x0, r.y0));
    rects
}

/// The skin coverage of the chosen faces: the Color Range skin classifier, smoothed, gated to
/// the face/neck/upper-chest region around those boxes, then turned into a layer mask that hides
/// everything else.
fn face_mask(doc: &Document, px: &[[f32; 4]], aw: usize, ah: usize, faces: &[Rect], coverage: FaceCoverage) -> Option<LayerMask> {
    let canvas = doc.bounds();
    if canvas.is_empty() || aw == 0 || ah == 0 {
        return None;
    }
    let raw_skin = selection::skin_tones(px, 40.0);
    let mut skin_data = matting::box_mean(&raw_skin, aw, ah, 1);
    let skin_bbox = clean_skin(&mut skin_data, aw, ah, canvas, faces, Rect::EMPTY, coverage);
    if skin_bbox.is_empty() {
        return None;
    }
    let field = Field::filled(canvas, aw, ah, skin_data).with_content(skin_bbox);
    let surface = field_mask_surface(&field, canvas, None);
    if surface.content_bounds().is_empty() {
        return None;
    }
    Some(hide_mask(surface))
}

/// What a face analysis found, shared by the mask command and the picker's scan.
struct FaceAnalysis {
    faces: usize,
    selected: usize,
    boxes: Vec<[i32; 4]>,
    mask: Option<LayerMask>,
    mode: FaceChoice,
    coverage: FaceCoverage,
}

/// Analyse the snapshot without the target layer's own colour effect, then optionally build the
/// soft skin mask. `ctx` carries progress and cancellation; the detector itself is a single call,
/// so cancellation is checked around the stages.
fn analyze_faces(
    doc: &Document,
    target: Option<LayerId>,
    choice: FaceChoice,
    coverage: FaceCoverage,
    override_rects: Option<&[Rect]>,
    build_mask: bool,
    ctx: &crate::jobs::JobCtx,
) -> Result<FaceAnalysis> {
    let canvas = doc.bounds();
    if canvas.is_empty() {
        return Err(EngineError::Other("the document has no pixels to analyse".into()));
    }
    let base = target.map(|id| without_layer(doc, id));
    let source = base.as_ref().unwrap_or(doc);
    let (aw, ah) = analysis_size(canvas);
    ctx.progress(0.05, "Preparing image");
    let px = photocraft_compose::render_reduced(source, aw as u32, ah as u32).px;
    if px.len() != aw.saturating_mul(ah) {
        return Err(EngineError::Other("the reduced analysis image has an unexpected size".into()));
    }
    ctx.check()?;
    ctx.progress(0.25, "Detecting faces");
    let found = face_rects_at(&px, aw, ah, override_rects);
    ctx.check()?;
    if found.is_empty() {
        return Err(EngineError::Other("no face found".into()));
    }
    let faces: Vec<Rect> = match choice {
        FaceChoice::All => found.clone(),
        FaceChoice::Main => found.iter().copied().max_by(|a, b| rect_area(*a).total_cmp(&rect_area(*b))).into_iter().collect(),
        FaceChoice::Index(n) => found.get(n.saturating_sub(1)).copied().into_iter().collect(),
    };
    if faces.is_empty() {
        return Err(EngineError::Other(format!(
            "face {} of {} not found",
            match choice {
                FaceChoice::Index(n) => n,
                _ => 0,
            },
            found.len()
        )));
    }
    ctx.progress(0.65, "Building mask");
    let mask = if build_mask { Some(face_mask(doc, &px, aw, ah, &faces, coverage).ok_or_else(|| EngineError::Other("no face found".into()))?) } else { None };
    ctx.check()?;
    ctx.progress(0.95, "Finalizing");
    let sx = canvas.width() as f32 / aw.max(1) as f32;
    let sy = canvas.height() as f32 / ah.max(1) as f32;
    let boxes = found
        .iter()
        .map(|r| {
            [
                canvas.x0 + (r.x0 as f32 * sx).round() as i32,
                canvas.y0 + (r.y0 as f32 * sy).round() as i32,
                (r.width() as f32 * sx).round() as i32,
                (r.height() as f32 * sy).round() as i32,
            ]
        })
        .collect();
    Ok(FaceAnalysis { faces: found.len(), selected: faces.len(), boxes, mask, mode: choice, coverage })
}

fn face_result(id: LayerId, a: FaceAnalysis) -> Value {
    json!({
        "layer": id.0,
        "faces": a.faces,
        "selected": a.selected,
        "mode": a.mode.id(),
        "coverage": a.coverage.id(),
        "boxes": a.boxes,
    })
}

/// Validate the explicit target before a job starts: only adjustment layers can take the mask.
fn face_target(s: &Session, id: LayerId) -> Result<()> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let layer = d.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
    if !matches!(layer.content, LayerContent::Adjustment(_)) {
        return Err(bad(
            FROM_FACE,
            format!(
                "layer \"{}\" is {} {} layer; a mask from a face needs an adjustment layer",
                layer.name,
                layer.content.article(),
                layer.content.kind_name()
            ),
        ));
    }
    Ok(())
}

fn from_face_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let mode = FaceChoice::parse(p)?;
    let coverage = FaceCoverage::parse(p)?;
    let id = crate::commands::layer_param(s, p)?;
    face_target(s, id)?;
    crate::jobs::edit_job(
        s,
        "Layer Mask from Face",
        move |doc, _, ctx| {
            let mut a = analyze_faces(doc, Some(id), mode, coverage, None, true, ctx)?;
            let mask = a.mask.take().ok_or_else(|| EngineError::Other("no face found".into()))?;
            doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?.mask = Some(mask);
            Ok(a)
        },
        move |a| face_result(id, a),
    )
}

#[cfg(test)]
fn from_face_cmd_with(s: &mut Session, p: &Value, override_rects: Option<&[Rect]>) -> Result<Value> {
    let mode = FaceChoice::parse(p)?;
    let coverage = FaceCoverage::parse(p)?;
    let id = crate::commands::layer_param(s, p)?;
    face_target(s, id)?;
    let doc = s.active().ok_or(EngineError::NoDocument)?.doc.clone();
    let mut a = analyze_faces(&doc, Some(id), mode, coverage, override_rects, true, &crate::jobs::JobCtx::new())?;
    let mask = a.mask.take().ok_or_else(|| EngineError::Other("no face found".into()))?;
    s.edit("Layer Mask from Face", |doc, _| {
        doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?.mask = Some(mask);
        Ok(())
    })?;
    Ok(face_result(id, a))
}

/// A fast, read-only scan for the face picker: returns every face box in document coordinates.
fn detect_faces_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let target = p.get("layer").and_then(Value::as_u64).map(LayerId);
    if let Some(id) = target
        && s.active().and_then(|d| d.doc.layer(id)).is_none()
    {
        return Err(EngineError::NoLayer(id));
    }
    let doc = s.active().ok_or(EngineError::NoDocument)?.doc.clone();
    crate::jobs::run(
        s,
        "Detect Faces",
        true,
        move |ctx| {
            let target = target.filter(|id| doc.layer(*id).is_some_and(|l| matches!(l.content, LayerContent::Adjustment(_))));
            analyze_faces(&doc, target, FaceChoice::All, FaceCoverage::FaceNeck, None, false, ctx)
        },
        move |_, a| Ok(json!({"faces": a.faces, "boxes": a.boxes})),
    )
}

#[cfg(test)]
fn detect_faces_with(s: &mut Session, p: &Value, override_rects: Option<&[Rect]>) -> Result<Value> {
    let target = p.get("layer").and_then(Value::as_u64).map(LayerId);
    if let Some(id) = target
        && s.active().and_then(|d| d.doc.layer(id)).is_none()
    {
        return Err(EngineError::NoLayer(id));
    }
    let doc = s.active().ok_or(EngineError::NoDocument)?.doc.clone();
    let target = target.filter(|id| doc.layer(*id).is_some_and(|l| matches!(l.content, LayerContent::Adjustment(_))));
    let a = analyze_faces(&doc, target, FaceChoice::All, FaceCoverage::FaceNeck, override_rects, false, &crate::jobs::JobCtx::new())?;
    Ok(json!({"faces": a.faces, "boxes": a.boxes}))
}

fn has_adjustment_layer(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    let layer = d.active_layer.and_then(|id| d.doc.layer(id)).ok_or("no active layer")?;
    if matches!(layer.content, LayerContent::Adjustment(_)) { Ok(()) } else { Err("the active layer is not an adjustment layer".into()) }
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: RUN,
            label: "Retouch AI: Run",
            menu: &[],
            shortcut: None,
            params: r##"{"tools":[{"id":"heal|dodgeBurn|skinTone|mattifier|skinMask|portraitVolumes|eyeBrilliance|eyeVessels|teeth|cleanHair|fabricVolumes|dustClean|backgroundClean|colorMatch","amount":0..100=100},…] (1..14, unique),"layer":id? (default active layer),"sampleAllLayers":bool=true,"referencePath":str? (colorMatch only),"selectionOnly":bool=false} → {"layers":[{"tool","name","layer","blend":"normal","opacity":1.0}],"analysis":{"faces","skin","subject","eyes","mouth","regions":{"skin":[x,y,w,h],"subject":[x,y,w,h]}},"skipped":[{"tool","reason"}]}"##,
            enabled: has_doc,
            run: run_cmd,
            journal: true,
        },
        CommandSpec {
            id: ANALYZE,
            label: "Retouch AI: Analyze",
            menu: &[],
            shortcut: None,
            params: r##"{"layer":id? (default active layer),"sampleAllLayers":bool=true} → {"faces","skin","subject","eyes","mouth","regions":{"skin":[x,y,w,h],"subject":[x,y,w,h]},"suggestions":[tool ids]}"##,
            enabled: has_doc,
            run: analyze_cmd,
            journal: false,
        },
        CommandSpec {
            id: FROM_FACE,
            label: "Layer Mask from Face",
            menu: &[],
            shortcut: None,
            params: r##"{"layer":id? (adjustment layer; default active layer),"faces":"all"|"main"|1..n="all","coverage":"face"|"faceNeck"="faceNeck"} → {"layer":id,"faces":count,"selected":count,"mode":"all"|"main"|n,"coverage":"face"|"faceNeck","boxes":[[x,y,w,h],…]}"##,
            enabled: has_adjustment_layer,
            run: from_face_cmd,
            journal: true,
        },
        CommandSpec {
            id: DETECT_FACES,
            label: "Detect Faces",
            menu: &[],
            shortcut: None,
            params: r##"{"layer":id? (adjustment layer to exclude from the analysis)} → {"faces":count,"boxes":[[x,y,w,h],…]} (read-only scan in reading order)"##,
            enabled: has_doc,
            run: detect_faces_cmd,
            journal: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use photocraft_color::{Color, ColorMode, SampleType};
    use photocraft_doc::Size;
    use photocraft_raster::from_rgba_into;
    use serde_json::json;

    use super::*;

    fn synthetic_session(depth: SampleType) -> Session {
        let size = Size::new(192, 144);
        let mut doc = Document::with_background("Retouch AI test", size, ColorMode::Rgb, depth, Color::WHITE);
        let fmt = doc.pixel_format();
        let bounds = doc.bounds();
        let w = bounds.width() as usize;
        let h = bounds.height() as usize;
        let ch = fmt.channels();
        let mut rgba = vec![[1.0f32, 1.0, 1.0, 1.0]; w * h];
        for y in 0..h {
            for x in 0..w {
                let dx = (x as f32 - 96.0) / 44.0;
                let dy = (y as f32 - 78.0) / 42.0;
                if dx * dx + dy * dy <= 1.0 {
                    rgba[y * w + x] = [0.80, 0.56, 0.43, 1.0];
                }
            }
        }
        // A few blemishes for Heal.
        for (cx, cy) in [(84, 68), (105, 76), (92, 88)] {
            for y in (cy - 1)..=(cy + 1) {
                for x in (cx - 1)..=(cx + 1) {
                    rgba[y * w + x] = [0.52, 0.30, 0.26, 1.0];
                }
            }
        }
        let mut native = Vec::with_capacity(rgba.len() * ch);
        let mut tmp = [0.0f32; 4];
        for p in &rgba {
            let _ = from_rgba_into(&fmt, *p, &mut tmp);
            native.extend_from_slice(&tmp[..ch]);
        }
        if let Some(surface) = doc.layers[0].surface_mut() {
            surface.write_region(bounds, &native);
        }
        let mut s = Session::new();
        s.add_document(doc, None);
        s
    }

    fn plain_session() -> Session {
        let doc = Document::with_background("Plain", Size::new(96, 80), ColorMode::Rgb, SampleType::U8, Color::WHITE);
        let mut s = Session::new();
        s.add_document(doc, None);
        s
    }

    fn layer_ids(s: &Session) -> Vec<LayerId> {
        s.active().unwrap().doc.walk().into_iter().map(|(_, _, l)| l.id).collect()
    }

    fn pixels_of(s: &Session, id: LayerId) -> Vec<f32> {
        let d = s.active().unwrap();
        d.doc.layer(id).unwrap().surface().unwrap().read_region(d.doc.bounds())
    }

    fn reference_png() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reference.png");
        let reference = Document::with_background("Reference", Size::new(40, 30), ColorMode::Rgb, SampleType::U8, Color::rgb(0.2, 0.4, 0.7));
        let bytes = photocraft_io::export(&reference, "png", &photocraft_io::ExportOptions::default()).unwrap().bytes;
        std::fs::write(&path, bytes).unwrap();
        (dir, path)
    }

    fn detected_face(score: f32, x: f32, y: f32, w: f32, h: f32) -> faces::Face {
        faces::Face { score, rect: [x, y, w, h], landmarks: [[x, y]; 5] }
    }

    #[test]
    fn face_dedupe_merges_overlaps_and_keeps_adjacent_faces() {
        // Two shifted boxes for one face, a small crop inside it, and a separate face beside it.
        let found = vec![
            detected_face(0.90, 0.0, 0.0, 100.0, 100.0),
            detected_face(0.80, 8.0, 8.0, 96.0, 96.0),
            detected_face(0.70, 35.0, 35.0, 25.0, 25.0),
            detected_face(0.60, 130.0, 0.0, 100.0, 100.0),
        ];
        let kept = dedupe_faces(found);
        assert_eq!(kept.len(), 2, "duplicates collapsed, adjacent faces kept: {kept:?}");
        assert!(kept.iter().any(|f| face_rect(f).x0 == 130));
        // The close-up's two shifted boxes (IoU ~0.32) are the same face.
        let closeup = vec![detected_face(0.685, 112.5, 347.9, 1097.7, 1198.0), detected_face(0.657, 256.7, -151.5, 1154.2, 1239.9)];
        assert_eq!(dedupe_faces(closeup).len(), 1);
        // The same close-up as the full-resolution detector reports it: four shifted copies.
        let full_res = vec![
            detected_face(0.600, 576.4, -149.2, 1782.0, 1753.0),
            detected_face(0.579, 368.0, 858.7, 1681.2, 1910.1),
            detected_face(0.575, 1476.9, 33.8, 1208.3, 1656.8),
            detected_face(0.569, 171.0, 156.1, 1676.7, 1925.0),
        ];
        assert_eq!(dedupe_faces(full_res).len(), 1);
    }

    #[test]
    fn skin_content_bounds_are_gated_to_faces() {
        let canvas = Rect::new(0, 0, 400, 300);
        // A wall-coloured background that the Color Range classifier calls skin everywhere.
        let mut data = vec![0.5f32; 400 * 300];
        let face = Rect::from_xywh(180, 120, 40, 40);
        let bounds = clean_skin(&mut data, 400, 300, canvas, &[face], Rect::EMPTY, FaceCoverage::FaceNeck);
        assert!(!bounds.is_empty());
        assert!(bounds.width() < 200 && bounds.height() < 200, "skin bbox must stay near the face: {bounds:?}");
        assert!(bounds.contains(200, 140));
        assert_eq!(data[0], 0.0, "skin outside the portrait region must be cleared");
        assert!(data[140 * 400 + 200] > 0.0);
        // Without a face, the subject bbox provides the spatial gate.
        let mut data = vec![0.5f32; 400 * 300];
        let bounds = clean_skin(&mut data, 400, 300, canvas, &[], Rect::from_xywh(80, 60, 120, 90), FaceCoverage::FaceNeck);
        assert!(bounds.width() < 200 && bounds.height() < 200, "subject-gated skin bbox: {bounds:?}");
        assert!(bounds.contains(120, 90));
        assert_eq!(data[0], 0.0);
    }

    #[test]
    fn no_document_is_an_error_and_disabled() {
        let mut s = Session::new();
        assert!(s.execute(RUN, json!({"tools":[{"id":"skinMask"}]})).is_err());
        assert!(s.execute(ANALYZE, json!({})).is_err());
        assert!(!s.is_enabled(RUN));
        assert!(!s.is_enabled(ANALYZE));
    }

    #[test]
    fn empty_and_invalid_tools_are_rejected() {
        let mut s = synthetic_session(SampleType::U8);
        for params in [
            json!({}),
            json!({"tools":[]}),
            json!({"tools":[{"id":"nope"}]}),
            json!({"tools":[{"id":"heal","amount":"lots"}]}),
            json!({"tools":[{"id":"heal"},{"id":"heal"}]}),
            json!({"tools":[{"id":"colorMatch"}],"referencePath":5}),
            json!({"tools":[{"id":"colorMatch"}],"referencePath":"/no/such/reference.png"}),
            json!({"tools":[{"id":"heal"}],"sampleAllLayers":"yes"}),
            json!({"tools":[{"id":"heal"}],"layer":999999}),
        ] {
            assert!(s.execute(RUN, params.clone()).is_err(), "{params}");
        }
        let too_many: Vec<Value> = (0..15).map(|_| json!({"id":"heal"})).collect();
        assert!(s.execute(RUN, json!({"tools":too_many})).is_err());
    }

    #[test]
    fn heal_and_color_match_without_a_reference_skips_color_match_only() {
        let mut s = synthetic_session(SampleType::U8);
        let r = s.execute(RUN, json!({"tools":[{"id":"heal"},{"id":"colorMatch"}]})).unwrap();
        assert_eq!(r["layers"].as_array().map(Vec::len), Some(1), "{r}");
        assert_eq!(r["layers"][0]["tool"], "heal");
        let skipped = r["skipped"].as_array().unwrap();
        assert_eq!(skipped.len(), 1, "{r}");
        assert_eq!(skipped[0]["tool"], "colorMatch");
        assert_eq!(skipped[0]["reason"], "colorMatch needs referencePath");
    }

    #[test]
    fn repeated_runs_keep_the_source_active_and_stack_above_old_results() {
        let mut s = synthetic_session(SampleType::U8);
        let source = s.active().unwrap().active_layer.unwrap();
        let first = s.execute(RUN, json!({"tools":[{"id":"skinMask"}]})).unwrap();
        assert_eq!(first["layers"].as_array().map(Vec::len), Some(1), "{first}");
        assert_eq!(s.active().unwrap().active_layer, Some(source), "the source must stay active after a run");
        let first_id = LayerId(first["layers"][0]["layer"].as_u64().unwrap());

        let second = s.execute(RUN, json!({"tools":[{"id":"skinMask"}]})).unwrap();
        assert_eq!(second["layers"].as_array().map(Vec::len), Some(1), "{second}");
        let second_id = LayerId(second["layers"][0]["layer"].as_u64().unwrap());
        assert_ne!(first_id, second_id);

        let d = s.active().unwrap();
        let stack: Vec<LayerId> = d.doc.walk().into_iter().map(|(_, _, l)| l.id).collect();
        assert_eq!(stack, vec![source, first_id, second_id], "the second run must stack above the first");
        assert_eq!(d.active_layer, Some(source), "no result layer may be left active");
    }

    #[test]
    fn results_clear_a_duplicate_background() {
        let mut s = synthetic_session(SampleType::U8);
        let source = s.active().unwrap().active_layer.unwrap();
        s.execute("layer.duplicate", json!({})).unwrap();
        s.execute("layer.select", json!({"layer": source.0})).unwrap();
        let r = s.execute(RUN, json!({"tools":[{"id":"skinMask"}]})).unwrap();
        assert_eq!(r["layers"].as_array().map(Vec::len), Some(1), "{r}");
        let id = LayerId(r["layers"][0]["layer"].as_u64().unwrap());
        assert_eq!(layer_ids(&s).last(), Some(&id), "the result must sit above the duplicate");
        assert_eq!(s.active().unwrap().active_layer, Some(source));
    }

    #[test]
    fn amount_zero_is_a_no_op_without_history() {
        let mut s = synthetic_session(SampleType::U8);
        let before = layer_ids(&s);
        let r = s.execute(RUN, json!({"tools":[{"id":"skinMask","amount":0}]})).unwrap();
        assert_eq!(r["layers"].as_array().map(Vec::len), Some(0));
        assert_eq!(r["skipped"][0]["reason"], "amount is 0");
        assert_eq!(layer_ids(&s), before);
        assert!(!s.active().unwrap().history.can_undo());
    }

    #[test]
    fn skin_mask_duplicates_the_source_and_adds_a_skin_mask() {
        let mut s = synthetic_session(SampleType::U8);
        let source = s.active().unwrap().active_layer.unwrap();
        let before = pixels_of(&s, source);
        let a = s.execute(ANALYZE, json!({})).unwrap();
        let skin_bbox = Rect::from_xywh(
            a["regions"]["skin"][0].as_i64().unwrap_or(0) as i32,
            a["regions"]["skin"][1].as_i64().unwrap_or(0) as i32,
            a["regions"]["skin"][2].as_u64().unwrap_or(0) as u32,
            a["regions"]["skin"][3].as_u64().unwrap_or(0) as u32,
        );
        let r = s.execute(RUN, json!({"tools":[{"id":"skinMask"}]})).unwrap();
        assert_eq!(r["layers"].as_array().map(Vec::len), Some(1));
        assert_eq!(r["layers"][0]["name"], "Retouch • Skin Mask");
        let id = LayerId(r["layers"][0]["layer"].as_u64().unwrap());
        let d = s.active().unwrap();
        let layer = d.doc.layer(id).unwrap();
        let mask = layer.mask.as_ref().expect("skinMask must have a mask");
        assert!(mask.value(96, 78) > 0.5, "centre of the skin ellipse must be revealed");
        assert!(mask.value(2, 2) < 0.01, "the plain background must stay hidden");
        // The copy is sparse: only the skin bbox holds pixels, not the whole source.
        let copy_bounds = layer.surface().unwrap().content_bounds();
        assert_eq!(copy_bounds, mask.surface.content_bounds());
        assert!(skin_bbox.contains_rect(&copy_bounds), "skinMask copy {copy_bounds:?} must stay in {skin_bbox:?}");
        assert!(copy_bounds.width() < d.doc.bounds().width());
        assert!(copy_bounds.height() < d.doc.bounds().height());
        // Duplicated pixels are the source's, unchanged inside the sparse copy.
        assert_eq!(layer.surface().unwrap().read_region(copy_bounds), d.doc.layer(source).unwrap().surface().unwrap().read_region(copy_bounds));
        assert_eq!(pixels_of(&s, source), before);
        assert_eq!(d.active_layer, Some(source), "the source layer stays selected after a run");
    }

    #[test]
    fn combo_creates_one_layer_per_tool_and_one_undo_restores_everything() {
        let mut s = synthetic_session(SampleType::U8);
        let source = s.active().unwrap().active_layer.unwrap();
        let before_layers = layer_ids(&s);
        let before_px = pixels_of(&s, source);
        let r = s.execute(RUN, json!({"tools":[{"id":"skinMask"},{"id":"skinTone","amount":80},{"id":"portraitVolumes","amount":70}]})).unwrap();
        assert_eq!(r["layers"].as_array().map(Vec::len), Some(3), "{r}");
        assert_eq!(r["skipped"].as_array().map(Vec::len), Some(0), "{r}");
        let d = s.active().unwrap();
        assert_eq!(d.doc.layer_count(), before_layers.len() + 3);
        assert_eq!(pixels_of(&s, source), before_px, "the source layer must not change");
        // Each result is above the source, named for its tool, and has a mask.
        let ids: Vec<u64> = r["layers"].as_array().unwrap().iter().map(|l| l["layer"].as_u64().unwrap()).collect();
        let source_pos = d.doc.walk().iter().position(|(_, _, l)| l.id == source).unwrap();
        for id in ids {
            let id = LayerId(id);
            let pos = d.doc.walk().iter().position(|(_, _, l)| l.id == id).unwrap();
            assert!(pos > source_pos);
            let l = d.doc.layer(id).unwrap();
            assert!(l.mask.is_some());
            assert_eq!(l.blend, BlendMode::Normal);
            assert_eq!(l.opacity, 1.0);
        }
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(layer_ids(&s), before_layers);
        assert_eq!(s.active().unwrap().active_layer, Some(source));
        assert_eq!(pixels_of(&s, source), before_px);
    }

    #[test]
    fn optional_face_tools_skip_cleanly_without_a_face() {
        let mut s = plain_session();
        let r = s.execute(RUN, json!({"tools":[{"id":"eyeBrilliance"},{"id":"eyeVessels"},{"id":"teeth"},{"id":"cleanHair"}]})).unwrap();
        assert_eq!(r["layers"].as_array().map(Vec::len), Some(0), "{r}");
        let skipped = r["skipped"].as_array().unwrap();
        assert_eq!(skipped.len(), 4);
        assert!(skipped.iter().all(|s| s["reason"].as_str().is_some_and(|r| !r.is_empty())));
        assert!(!s.active().unwrap().history.can_undo(), "a fully skipped run must not journal");
    }

    #[test]
    fn every_tool_runs_or_skips_cleanly_and_one_undo_restores() {
        let (_dir, path) = reference_png();
        let mut s = synthetic_session(SampleType::U8);
        let source = s.active().unwrap().active_layer.unwrap();
        let before_layers = layer_ids(&s);
        let before = pixels_of(&s, source);
        let tools: Vec<Value> = Tool::ALL.iter().map(|t| json!({"id":t.id()})).collect();
        let r = s.execute(RUN, json!({"tools":tools,"referencePath":path.to_str().unwrap()})).unwrap();
        let layers = r["layers"].as_array().unwrap();
        let skipped = r["skipped"].as_array().unwrap();
        assert_eq!(layers.len() + skipped.len(), Tool::ALL.len(), "{r}");
        assert!(skipped.iter().all(|s| s["reason"].as_str().is_some_and(|r| !r.is_empty())));
        let mut ids: Vec<&str> = layers.iter().filter_map(|l| l["tool"].as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), layers.len(), "one layer per requested tool");
        assert_eq!(pixels_of(&s, source), before);
        if !layers.is_empty() {
            s.execute("edit.undo", json!({})).unwrap();
            assert_eq!(layer_ids(&s), before_layers);
            assert_eq!(s.active().unwrap().active_layer, Some(source));
        }
    }

    #[test]
    fn analyze_does_not_touch_pixels_or_history() {
        let mut s = synthetic_session(SampleType::U8);
        let source = s.active().unwrap().active_layer.unwrap();
        let before_px = pixels_of(&s, source);
        let before_layers = layer_ids(&s);
        let r = s.execute(ANALYZE, json!({})).unwrap();
        assert_eq!(r["faces"], 0, "{r}");
        assert_eq!(r["skin"], true, "{r}");
        let region = r["regions"]["skin"].as_array().expect("skin region");
        assert_eq!(region.len(), 4);
        assert!(region[2].as_u64().unwrap_or(u64::MAX) < 192);
        assert!(region[3].as_u64().unwrap_or(u64::MAX) < 144);
        assert!(r["suggestions"].as_array().is_some_and(|a| a.iter().any(|v| v == "skinMask")));
        assert_eq!(pixels_of(&s, source), before_px);
        assert_eq!(layer_ids(&s), before_layers);
        assert!(!s.active().unwrap().history.can_undo());
    }

    #[test]
    fn skin_tone_layer_bounds_match_the_tight_skin_region() {
        let mut s = synthetic_session(SampleType::U8);
        let a = s.execute(ANALYZE, json!({})).unwrap();
        let expected = Rect::from_xywh(
            a["regions"]["skin"][0].as_i64().unwrap_or(0) as i32,
            a["regions"]["skin"][1].as_i64().unwrap_or(0) as i32,
            a["regions"]["skin"][2].as_u64().unwrap_or(0) as u32,
            a["regions"]["skin"][3].as_u64().unwrap_or(0) as u32,
        );
        let r = s.execute(RUN, json!({"tools":[{"id":"skinTone","amount":60}]})).unwrap();
        assert_eq!(r["layers"].as_array().map(Vec::len), Some(1), "{r}");
        let id = LayerId(r["layers"][0]["layer"].as_u64().unwrap());
        let d = s.active().unwrap();
        let layer = d.doc.layer(id).unwrap();
        let bounds = layer.surface().unwrap().content_bounds();
        assert_eq!(bounds, expected, "the layer must store only the skin bbox");
        let mask = layer.mask.as_ref().expect("skinTone must have a mask");
        assert!(!mask.surface.content_bounds().is_empty());
        assert!(mask.surface.content_bounds().width() <= expected.width());
        assert!(mask.surface.content_bounds().height() <= expected.height());
    }

    #[test]
    fn heal_detects_and_inpaints_synthetic_blemishes() {
        let mut s = synthetic_session(SampleType::U8);
        let r = s.execute(RUN, json!({"tools":[{"id":"heal","amount":100}]})).unwrap();
        assert_eq!(r["layers"].as_array().map(Vec::len), Some(1), "{r}");
        let id = LayerId(r["layers"][0]["layer"].as_u64().unwrap());
        let d = s.active().unwrap();
        let l = d.doc.layer(id).unwrap();
        let mask = l.mask.as_ref().unwrap();
        // A blemish is revealed; clear skin away from the defect is hidden.
        assert!(mask.value(84, 68) > 0.2);
        assert!(mask.value(96, 78) < 0.01);
    }

    #[test]
    fn color_match_reads_a_reference_file_and_masks_its_written_region() {
        let (_dir, path) = reference_png();
        let mut s = synthetic_session(SampleType::U8);
        let source = s.active().unwrap().active_layer.unwrap();
        let before = pixels_of(&s, source);
        let r = s.execute(RUN, json!({"tools":[{"id":"colorMatch","amount":75}],"referencePath": path.to_str().unwrap()})).unwrap();
        assert_eq!(r["layers"].as_array().map(Vec::len), Some(1), "{r}");
        assert_eq!(r["layers"][0]["name"], "Retouch • Color Match");
        let id = LayerId(r["layers"][0]["layer"].as_u64().unwrap());
        let d = s.active().unwrap();
        let l = d.doc.layer(id).unwrap();
        let mask = l.mask.as_ref().expect("colorMatch must have a mask");
        assert!(mask.value(2, 2) > 0.9, "no selection means the whole written bbox is revealed");
        assert_eq!(mask.surface.content_bounds(), l.surface().unwrap().content_bounds());
        assert_eq!(pixels_of(&s, source), before);
    }

    #[test]
    fn works_at_16_bit_depth() {
        let mut s = synthetic_session(SampleType::U16);
        let r = s.execute(RUN, json!({"tools":[{"id":"skinMask"},{"id":"skinTone","amount":50}]})).unwrap();
        assert_eq!(r["layers"].as_array().map(Vec::len), Some(2), "{r}");
        assert_eq!(s.active().unwrap().doc.depth, SampleType::U16);
    }

    #[test]
    fn selection_only_restricts_skin_mask() {
        let mut s = synthetic_session(SampleType::U8);
        s.execute("select.rect", json!({"x":60,"y":50,"width":20,"height":20})).unwrap();
        let r = s.execute(RUN, json!({"tools":[{"id":"skinMask"}],"selectionOnly":true})).unwrap();
        let id = LayerId(r["layers"][0]["layer"].as_u64().unwrap());
        let d = s.active().unwrap();
        let mask = d.doc.layer(id).unwrap().mask.as_ref().unwrap();
        assert!(mask.value(65, 55) > 0.2);
        assert!(mask.value(120, 78) < 0.01, "skin outside the selection must be hidden");
    }

    // ---------- layer.layerMask.fromFace ----------

    fn adjustment_session() -> (Session, LayerId) {
        let mut s = synthetic_session(SampleType::U8);
        // A grayscale adjustment on top proves the analysis excludes the mask target's own
        // colour effect: the hidden layer would otherwise desaturate the skin away.
        let r = s.execute("layer.newAdjustmentLayer.basicColor", json!({"saturation": -100})).unwrap();
        let id = LayerId(r["layer"].as_u64().unwrap());
        (s, id)
    }

    fn add_skin_blob(doc: &mut Document, cx: i32, cy: i32, rx: i32, ry: i32) {
        let fmt = doc.pixel_format();
        let bounds = doc.bounds();
        let ch = fmt.channels();
        let Some(surface) = doc.layers[0].surface_mut() else { return };
        let native = surface.read_region(bounds);
        let w = bounds.width() as usize;
        let mut rgba: Vec<[f32; 4]> = native.chunks_exact(ch).map(|p| to_rgba(&fmt, p)).collect();
        for y in 0..bounds.height() {
            for x in 0..bounds.width() {
                let dx = (x as f32 - cx as f32) / rx.max(1) as f32;
                let dy = (y as f32 - cy as f32) / ry.max(1) as f32;
                if dx * dx + dy * dy <= 1.0 {
                    rgba[y as usize * w + x as usize] = [0.80, 0.56, 0.43, 1.0];
                }
            }
        }
        let mut out = Vec::with_capacity(rgba.len() * ch);
        let mut tmp = [0.0f32; 4];
        for p in &rgba {
            let _ = from_rgba_into(&fmt, *p, &mut tmp);
            out.extend_from_slice(&tmp[..ch]);
        }
        surface.write_region(bounds, &out);
    }

    #[test]
    fn from_face_requires_a_document_a_pixel_layer_and_valid_params() {
        let mut s = Session::new();
        assert!(s.execute(FROM_FACE, json!({})).is_err());
        assert!(!s.is_enabled(FROM_FACE));

        let mut s = synthetic_session(SampleType::U8);
        assert!(s.execute(FROM_FACE, json!({})).is_err(), "the background is a pixel layer");
        assert!(!s.is_enabled(FROM_FACE));

        let (mut s, id) = adjustment_session();
        assert!(s.is_enabled(FROM_FACE));
        for params in [json!({"faces":"left"}), json!({"faces":7}), json!({"layer":999999}), json!({"layer":"nope"})] {
            assert!(s.execute(FROM_FACE, params.clone()).is_err(), "{params}");
        }
        // An explicit adjustment layer works even while a pixel layer is active.
        let background = s.active().unwrap().doc.layers[0].id;
        s.execute("layer.select", json!({"layer": background.0})).unwrap();
        let r = from_face_cmd_with(&mut s, &json!({"layer": id.0}), Some(&[Rect::from_xywh(70, 48, 52, 58)])).unwrap();
        assert_eq!(r["layer"], id.0);
    }

    #[test]
    fn from_face_without_a_face_errors_and_keeps_the_old_mask() {
        let (mut s, id) = adjustment_session();
        let mut old = LayerMask::hide_all();
        old.surface.write_region(Rect::from_xywh(3, 4, 1, 1), &[0.25]);
        old.surface.prune();
        let mut doc = (*s.active().unwrap().doc).clone();
        doc.layer_mut(id).unwrap().mask = Some(old.clone());
        s.active_mut().unwrap().doc = std::sync::Arc::new(doc);
        let label = s.active().unwrap().history.undo_label().map(str::to_string);

        let err = from_face_cmd_with(&mut s, &json!({"layer": id.0}), Some(&[])).unwrap_err();
        match err {
            EngineError::Other(msg) => assert_eq!(msg, "no face found"),
            other => panic!("expected `no face found`, got {other:?}"),
        }
        let d = s.active().unwrap();
        assert_eq!(d.doc.layer(id).unwrap().mask.as_ref(), Some(&old), "a failed analysis must not touch the old mask");
        assert_eq!(d.history.undo_label().map(str::to_string), label, "a failed analysis must not journal");
    }

    #[test]
    fn from_face_masks_the_face_neck_and_chest_in_one_undo_step() {
        let (mut s, id) = adjustment_session();
        // A selection must be ignored and left in place.
        s.execute("select.rect", json!({"x":0,"y":0,"width":10,"height":10})).unwrap();
        let before_content = s.active().unwrap().doc.layer(id).unwrap().content.clone();
        let before_mask = s.active().unwrap().doc.layer(id).unwrap().mask.clone();
        let before_layers = layer_ids(&s);
        let label = s.active().unwrap().history.undo_label().map(str::to_string);

        let r = from_face_cmd_with(&mut s, &json!({"layer": id.0}), Some(&[Rect::from_xywh(70, 48, 52, 58)])).unwrap();
        assert_eq!(r["layer"], id.0);
        assert_eq!(r["faces"], 1);
        assert_eq!(r["mode"], "all");
        {
            let d = s.active().unwrap();
            let layer = d.doc.layer(id).unwrap();
            assert_eq!(layer.content, before_content, "the adjustment itself must not change");
            assert_eq!(layer_ids(&s), before_layers, "no layer is added or removed");
            assert!(d.doc.selection.is_some(), "the selection must not be consumed");
            let mask = layer.mask.as_ref().expect("fromFace must install a mask");
            assert!(mask.value(96, 78) > 0.5, "the face skin must be revealed");
            assert!(mask.value(96, 110) > 0.05, "the neck/upper chest must be revealed");
            assert!(mask.value(4, 4) < 0.01, "the background must stay hidden");
            assert!(mask.value(96, 20) < 0.01, "hair above the face must stay hidden");
            assert_eq!(d.history.undo_label(), Some("Layer Mask from Face"), "exactly one named step");
        }
        s.execute("edit.undo", json!({})).unwrap();
        let d = s.active().unwrap();
        assert_eq!(d.doc.layer(id).unwrap().mask, before_mask, "one undo restores the mask the layer had before");
        assert_eq!(d.history.undo_label().map(str::to_string), label);
    }

    #[test]
    fn from_face_coverage_face_omits_the_neck() {
        let (mut s, id) = adjustment_session();
        let rect = Rect::from_xywh(70, 48, 52, 58);
        let r = from_face_cmd_with(&mut s, &json!({"layer": id.0, "coverage": "face"}), Some(&[rect])).unwrap();
        assert_eq!(r["coverage"], "face");
        let d = s.active().unwrap();
        let mask = d.doc.layer(id).unwrap().mask.as_ref().unwrap();
        assert!(mask.value(96, 78) > 0.5, "the face skin stays");
        assert!(mask.value(96, 125) < 0.02, "the neck/chest is outside the face-only gate");
    }

    #[test]
    fn from_face_index_uses_reading_order() {
        let (mut s, id) = adjustment_session();
        let mut doc = (*s.active().unwrap().doc).clone();
        add_skin_blob(&mut doc, 18, 100, 10, 10);
        s.active_mut().unwrap().doc = std::sync::Arc::new(doc);
        let big = Rect::from_xywh(70, 45, 52, 60);
        let small = Rect::from_xywh(8, 90, 20, 20);

        // Left to right: the small box is face 1, the big one face 2.
        let r = from_face_cmd_with(&mut s, &json!({"layer": id.0, "faces": 2}), Some(&[big, small])).unwrap();
        assert_eq!(r["mode"], "2");
        let d = s.active().unwrap();
        let mask = d.doc.layer(id).unwrap().mask.as_ref().unwrap();
        assert!(mask.value(96, 78) > 0.5, "face 2 is the larger, right-hand face");
        assert!(mask.value(18, 100) < 0.02, "face 1 stays out");

        let _ = from_face_cmd_with(&mut s, &json!({"layer": id.0, "faces": 1}), Some(&[big, small])).unwrap();
        let d = s.active().unwrap();
        let mask = d.doc.layer(id).unwrap().mask.as_ref().unwrap();
        assert!(mask.value(18, 100) > 0.2, "face 1 is the left-hand face");
        assert!(mask.value(96, 78) < 0.05, "face 2 stays out");
    }

    #[test]
    fn detect_faces_returns_boxes_and_touches_nothing() {
        let (mut s, id) = adjustment_session();
        let before_mask = s.active().unwrap().doc.layer(id).unwrap().mask.clone();
        let label = s.active().unwrap().history.undo_label().map(str::to_string);
        let big = Rect::from_xywh(70, 45, 52, 60);
        let small = Rect::from_xywh(8, 90, 20, 20);
        let r = detect_faces_with(&mut s, &json!({"layer": id.0}), Some(&[big, small])).unwrap();
        assert_eq!(r["faces"], 2);
        assert_eq!(r["boxes"], json!([[8, 90, 20, 20], [70, 45, 52, 60]]), "boxes are in reading order");
        let d = s.active().unwrap();
        assert_eq!(d.doc.layer(id).unwrap().mask, before_mask, "a scan never changes the mask");
        assert_eq!(d.history.undo_label().map(str::to_string), label, "a scan is not a history step");
    }

    #[test]
    fn from_face_rejects_bad_coverage_and_face_numbers() {
        let (mut s, id) = adjustment_session();
        for params in [json!({"layer": id.0, "coverage": "hair"}), json!({"layer": id.0, "faces": 0}), json!({"layer": id.0, "faces": -1})] {
            assert!(from_face_cmd_with(&mut s, &params, Some(&[Rect::from_xywh(70, 48, 52, 58)])).is_err(), "{params}");
        }
    }

    #[test]
    fn from_face_main_picks_the_largest_box_and_all_replaces_the_mask() {
        let (mut s, id) = adjustment_session();
        // A second face away from the first face's skin gate (which reaches 0.65 face widths out).
        let mut doc = (*s.active().unwrap().doc).clone();
        add_skin_blob(&mut doc, 18, 100, 10, 10);
        s.active_mut().unwrap().doc = std::sync::Arc::new(doc);
        let big = Rect::from_xywh(70, 45, 52, 60);
        let small = Rect::from_xywh(8, 90, 20, 20);

        let r = from_face_cmd_with(&mut s, &json!({"layer": id.0, "faces": "main"}), Some(&[big, small])).unwrap();
        assert_eq!(r["faces"], 2, "the count reports every detected face");
        assert_eq!(r["mode"], "main");
        {
            let d = s.active().unwrap();
            let mask = d.doc.layer(id).unwrap().mask.as_ref().unwrap();
            assert!(mask.value(96, 78) > 0.5, "the largest face is masked");
            assert!(mask.value(18, 100) < 0.02, "the smaller face stays out in main mode");
        }

        let r = from_face_cmd_with(&mut s, &json!({"layer": id.0}), Some(&[big, small])).unwrap();
        assert_eq!(r["mode"], "all");
        let d = s.active().unwrap();
        let mask = d.doc.layer(id).unwrap().mask.as_ref().unwrap();
        assert!(mask.value(18, 100) > 0.2, "all mode replaces the old mask and covers every face");
    }
}
