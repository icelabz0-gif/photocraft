//! [`InferenceBackend`] trait and its two built-in implementations:
//! [`LocalBackend`] (Ollama / llama.cpp REST API) and [`ClassicalFallback`]
//! (always returns [`MlError::Unsupported`]; the engine uses classical
//! algorithms instead).

use crate::{MlError, Task, request::{InferenceRequest, InferenceResult}};

// ─────────────────────────────────────────────────────────────────────────────
// Trait
// ─────────────────────────────────────────────────────────────────────────────

/// A backend that can execute AI inference tasks.
///
/// # Contract
/// - Every method must return `Err` on any bad input or runtime failure;
///   it must never panic (rule 1, `AGENTS.md`).
/// - Implementations are `Send + Sync` so the engine dispatches calls onto
///   rayon's thread pool.
/// - On `wasm32` the `native` feature is absent; callers get
///   [`ClassicalFallback`] which immediately returns `Err(Unsupported)`.
pub trait InferenceBackend: Send + Sync {
    /// Short identifier used in logs and the Jobs panel (e.g. `"local"`).
    fn id(&self) -> &str;

    /// Returns `true` when this backend can attempt `task`.  The engine may
    /// still receive `Err` at run time (network failure, model not loaded…).
    fn supports(&self, task: Task) -> bool;

    /// Run an inference request.  Blocking; must be called from a thread pool
    /// thread, never the UI thread.
    ///
    /// # Errors
    /// Returns [`MlError`] on any failure.  The document is never modified
    /// here; callers commit results as a `Transaction`.
    fn run(&self, req: InferenceRequest) -> Result<InferenceResult, MlError>;

    /// Optional liveness check.  Returns `Ok(())` when the backend is
    /// reachable.  The engine calls this from Preferences › AI › "Test
    /// connection".
    fn ping(&self) -> Result<(), MlError> {
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Classical fallback (always returns Unsupported)
// ─────────────────────────────────────────────────────────────────────────────

/// A no-op backend.  Every [`InferenceBackend::run`] call returns
/// [`MlError::Unsupported`] so the engine falls back to its built-in
/// classical algorithms (PatchMatch inpaint, HSV skin heuristic, …).
#[derive(Clone, Copy, Debug, Default)]
pub struct ClassicalFallback;

impl InferenceBackend for ClassicalFallback {
    fn id(&self) -> &str { "classical" }
    fn supports(&self, _task: Task) -> bool { false }
    fn run(&self, _req: InferenceRequest) -> Result<InferenceResult, MlError> {
        Err(MlError::Unsupported)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Local server backend (Ollama / llama.cpp)
// ─────────────────────────────────────────────────────────────────────────────

/// Backend that sends inference requests to a locally running model server
/// (Ollama, llama.cpp serve, or any server that exposes the same REST
/// surface).
#[derive(Clone, Debug)]
pub struct LocalBackend {
    /// Base URL of the server, e.g. `"http://127.0.0.1:11434"`.
    pub base_url: String,
    /// Model name passed in every request (e.g. `"llava"`, `"sdxl-inpaint"`).
    pub model: String,
    /// Request timeout in seconds.
    pub timeout_secs: u32,
}

impl LocalBackend {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>, timeout_secs: u32) -> Self {
        Self { base_url: base_url.into(), model: model.into(), timeout_secs }
    }

    /// Encode a region of `surface` as PNG (base64-encoded string).
    fn encode_png(surface: &photocraft_raster::Surface, bounds: photocraft_geom::Rect) -> Result<String, MlError> {
        let (w, h) = (bounds.width() as usize, bounds.height() as usize);
        if w == 0 || h == 0 {
            return Err(MlError::BadParam("image has zero dimension".into()));
        }
        let mut rgba8_tuples = vec![[0u8; 4]; w * h];
        surface.read_rgba8_into(bounds, &mut rgba8_tuples);
        let mut raw = Vec::with_capacity(w * h * 4);
        for px in rgba8_tuples {
            raw.extend_from_slice(&px);
        }
        let img = photocraft_codecs::Image::from_u8(w as u32, h as u32, photocraft_codecs::ChannelLayout::Rgba, raw)
            .map_err(|e| MlError::InvalidOutput(e.to_string()))?;
        let bytes = photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &photocraft_codecs::EncodeOptions::default())
            .map_err(|e| MlError::InvalidOutput(e.to_string()))?;
        Ok(base64_encode(&bytes))
    }

    /// Decode a base64-encoded PNG into a surface covering `target_rect`.
    fn decode_png(b64: &str, target_rect: photocraft_geom::Rect) -> Result<photocraft_raster::Surface, MlError> {
        let bytes = base64_decode(b64)?;
        let img = photocraft_codecs::decode(&bytes).map_err(|e| MlError::InvalidOutput(e.to_string()))?;
        let rgba8 = img.to_rgba8();
        let mut surf = photocraft_raster::Surface::new(photocraft_color::PixelFormat::RGBA8);
        surf.write_interleaved(target_rect, &rgba8);
        Ok(surf)
    }
}

impl InferenceBackend for LocalBackend {
    fn id(&self) -> &str { "local" }

    fn supports(&self, _task: Task) -> bool {
        cfg!(feature = "native")
    }

    fn run(&self, req: InferenceRequest) -> Result<InferenceResult, MlError> {
        #[cfg(not(feature = "native"))]
        { let _ = req; return Err(MlError::Unsupported); }

        #[cfg(feature = "native")]
        { self.run_native(req) }
    }

    fn ping(&self) -> Result<(), MlError> {
        #[cfg(not(feature = "native"))]
        return Err(MlError::Unsupported);

        #[cfg(feature = "native")]
        {
            let url = format!("{}/api/tags", self.base_url.trim_end_matches('/'));
            let client = make_client(self.timeout_secs)?;
            client.get(&url).send().map_err(|e| MlError::Network(e.to_string()))?;
            Ok(())
        }
    }
}

// ─── Native-only implementation ──────────────────────────────────────────────

#[cfg(feature = "native")]
impl LocalBackend {
    fn run_native(&self, req: InferenceRequest) -> Result<InferenceResult, MlError> {
        let img_b64 = Self::encode_png(&req.image, req.region)?;
        let mask_b64 = req.mask.as_ref().map(|m| Self::encode_png(m, req.region)).transpose()?;

        let mut body = serde_json::json!({
            "model":  self.model,
            "task":   req.task.api_name(),
            "stream": false,
            "options": { "seed": req.seed.unwrap_or(0) },
            "images": [img_b64],
            "strength": req.strength,
        });
        if let Some(mask) = mask_b64 {
            body["mask"] = serde_json::Value::String(mask);
        }
        if let Some(p) = &req.prompt {
            body["prompt"] = serde_json::Value::String(p.clone());
        }
        if let Some(np) = &req.negative_prompt {
            body["negative_prompt"] = serde_json::Value::String(np.clone());
        }
        if req.scale > 1 {
            body["scale"] = serde_json::json!(req.scale);
        }

        let url = format!("{}/api/generate", self.base_url.trim_end_matches('/'));
        let client = make_client(self.timeout_secs)?;

        let resp = client
            .post(&url)
            .json(&body)
            .send()
            .map_err(|e| MlError::Network(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let text = resp.text().unwrap_or_default();
            return Err(MlError::Response(format!("HTTP {status}: {text}")));
        }

        let json: serde_json::Value = resp.json().map_err(|e| MlError::Response(e.to_string()))?;

        let response_b64 = json
            .get("response")
            .and_then(|v| v.as_str())
            .ok_or_else(|| MlError::Response("missing `response` field in server reply".into()))?;

        let surf = Self::decode_png(response_b64, req.region)?;
        let result = match req.task {
            Task::SkinSegment | Task::SubjectSegment | Task::BackgroundMatte => {
                InferenceResult::Mask(surf)
            }
            _ => {
                InferenceResult::Pixels(surf)
            }
        };
        Ok(result)
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

#[cfg(feature = "native")]
fn make_client(timeout_secs: u32) -> Result<reqwest::blocking::Client, MlError> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(u64::from(timeout_secs)))
        .build()
        .map_err(|e| MlError::Network(e.to_string()))
}

/// Public base64 encoder (for tests).
pub fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 { out.push(ALPHABET[((n >> 6) & 0x3F) as usize] as char); } else { out.push('='); }
        if chunk.len() > 2 { out.push(ALPHABET[(n & 0x3F) as usize] as char); } else { out.push('='); }
    }
    out
}

/// Public base64 decoder (for tests).
pub fn base64_decode(s: &str) -> Result<Vec<u8>, MlError> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            b'=' | b'\n' | b'\r' | b' ' => None,
            _ => None,
        }
    }
    let bytes: Vec<u8> = s.bytes().filter(|b| !matches!(b, b'\n' | b'\r' | b' ')).collect();
    if bytes.len() % 4 != 0 {
        return Err(MlError::Response("invalid base64 length".into()));
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let (a, b, c, d) = (
            val(chunk[0]).ok_or_else(|| MlError::Response("invalid base64 char".into()))?,
            val(chunk[1]).ok_or_else(|| MlError::Response("invalid base64 char".into()))?,
            chunk[2],
            chunk[3],
        );
        let n = (u32::from(a) << 18) | (u32::from(b) << 12);
        out.push((n >> 16) as u8);
        if chunk[2] != b'=' {
            let cv = val(c).ok_or_else(|| MlError::Response("invalid base64 char".into()))?;
            let n2 = n | (u32::from(cv) << 6);
            out.push((n2 >> 8) as u8);
            if chunk[3] != b'=' {
                let dv = val(d).ok_or_else(|| MlError::Response("invalid base64 char".into()))?;
                out.push((n2 | u32::from(dv)) as u8);
            }
        }
    }
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classical_fallback_is_unsupported() {
        let fb = ClassicalFallback;
        assert!(!fb.supports(Task::Inpaint));
        assert!(matches!(fb.run(make_dummy_req(Task::Inpaint)), Err(MlError::Unsupported)));
    }

    #[test]
    fn base64_roundtrip() {
        let data: Vec<u8> = (0u8..=255).collect();
        let encoded = base64_encode(&data);
        let decoded = base64_decode(&encoded).expect("decode should succeed");
        assert_eq!(data, decoded);
    }

    fn make_dummy_req(task: Task) -> InferenceRequest {
        use photocraft_geom::Rect;
        let rect = Rect::new(0, 0, 1, 1);
        let surface = photocraft_raster::Surface::new(photocraft_color::PixelFormat::RGBA8);
        InferenceRequest {
            task,
            image: surface,
            mask: None,
            prompt: None,
            negative_prompt: None,
            seed: Some(42),
            strength: 0.85,
            scale: 1,
            region: rect,
        }
    }
}
