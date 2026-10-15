//! Request and result types shared by all backends.

use photocraft_geom::Rect;
use photocraft_raster::Surface;

use crate::Task;

/// Everything a backend needs to carry out one inference call.
///
/// Pixel data is passed as owned [`Surface`] values so the backend can hand
/// them off to a thread without borrowing the document snapshot.
pub struct InferenceRequest {
    /// Which task to perform.
    pub task: Task,
    /// Source image.  Format must be RGBA-f32 or the backend will return
    /// [`crate::MlError::BadParam`].
    pub image: Surface,
    /// Optional single-channel (grey-f32) alpha mask.  A fully-opaque pixel
    /// (value 1.0) means "this area should be replaced / processed".
    /// Not all tasks require a mask; backends silently ignore it when unused.
    pub mask: Option<Surface>,
    /// Optional text guidance (prompt).  Backends that don't support text
    /// guidance ignore this field.
    pub prompt: Option<String>,
    /// Optional negative text guidance.
    pub negative_prompt: Option<String>,
    /// Reproducibility seed.  `None` = backend chooses.
    pub seed: Option<u64>,
    /// 0.0–1.0 how strongly the model reshapes the source (for inpainting).
    /// Backends that don't support this field treat it as 1.0.
    pub strength: f32,
    /// Upscale factor (2 or 4); only meaningful for [`Task::Upscale`].
    pub scale: u8,
    /// Document-coordinate bounding box of the region being processed.
    /// Used for size caps and region-aware tiling; not sent to the server.
    pub region: Rect,
}

/// The output of a successful inference call.
pub enum InferenceResult {
    /// Pixel data in the same format as the request image.
    Pixels(Surface),
    /// A single-channel (grey-f32) mask.  Values 0.0 = background, 1.0 = subject.
    Mask(Surface),
}

