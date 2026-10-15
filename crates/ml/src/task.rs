//! Task identifiers sent in requests so the server can route them.

/// A named inference task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Task {
    /// Fill a masked region (inpainting / object removal / generative fill).
    Inpaint,
    /// Identify skin pixels and return a mask.
    SkinSegment,
    /// Segment the primary subject and return a mask.
    SubjectSegment,
    /// Separate the subject from its background; return an alpha matte.
    BackgroundMatte,
    /// Super-resolution upscale (2× or 4×).
    Upscale,
    /// Colorise a grayscale image.
    Colorize,
}

impl Task {
    /// The task name sent in API requests (Ollama-style model routing header).
    pub fn api_name(self) -> &'static str {
        match self {
            Self::Inpaint => "inpaint",
            Self::SkinSegment => "skin_segment",
            Self::SubjectSegment => "subject_segment",
            Self::BackgroundMatte => "background_matte",
            Self::Upscale => "upscale",
            Self::Colorize => "colorize",
        }
    }
}

