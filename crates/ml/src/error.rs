//! Typed error returned by every inference call.

use thiserror::Error;

/// Every failure mode an [`crate::InferenceBackend`] can report.
#[derive(Debug, Error)]
pub enum MlError {
    /// The backend or this wasm build does not support this task.
    #[error("AI inference is not supported in this build or backend configuration")]
    Unsupported,

    /// The local server was unreachable or returned a non-success HTTP status.
    #[error("AI backend network error: {0}")]
    Network(String),

    /// The server replied but its response could not be decoded.
    #[error("AI backend response error: {0}")]
    Response(String),

    /// The job was cancelled by the user before it completed.
    #[error("AI inference cancelled")]
    Cancelled,

    /// The requested region exceeds the configured megapixel cap.
    #[error("region is too large for AI inference ({mp:.1} MP > {cap:.1} MP cap)")]
    RegionTooLarge { mp: f32, cap: f32 },

    /// A parameter value was out of range or missing.
    #[error("bad AI parameter: {0}")]
    BadParam(String),

    /// The server returned a successful response but the image data was invalid.
    #[error("AI backend returned invalid image data: {0}")]
    InvalidOutput(String),
}

impl MlError {
    /// True when a retry (different seed, smaller region) might succeed.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Network(_) | Self::Response(_))
    }
}

