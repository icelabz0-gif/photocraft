# Requirements & Software Requirements Specification: AI Inline Editing

> **Document ID:** SRS-AI-INLINE-001  
> **Status:** Draft / Active Implementation  
> **Scope:** Local Server First (Ollama, llama.cpp), Remote Hosted VLM Deferred  
> **Target:** PhotoCraft Engine & UI

---

## 1. Executive Summary

This specification defines inline AI-assisted image editing inside PhotoCraft without cloud dependencies. Editing operations run against configurable local LLM/VLM inference servers (such as Ollama or `llama.cpp` `--server`), with zero data egress. When local servers are offline or unconfigured, operations fall back gracefully to deterministic classical algorithms (PatchMatch inpainting, bilateral/Gaussian smoothing).

Remote hosted VLMs (OpenAI, Stability AI, Replicate) are specified in later phases (MVP-4+) and remain dormant until explicitly enabled by user configuration.

---

## 2. Core Architecture & Layering

Following PhotoCraft's strict architectural layering:

```text
L6  ui-egui (Preferences dialog › AI, on-canvas feedback, before/after toggle)
L5  photocraft-engine (Commands: ai.removeObject, ai.skinEnhance, ai.pingBackend)
L4  photocraft-ml (InferenceBackend trait, LocalBackend, ClassicalFallback)
L0  photocraft-codecs, photocraft-raster, photocraft-color, photocraft-geom
```

- **Layer 4 (`photocraft-ml`):** Pure-Rust backend interface; HTTP client feature-gated behind `native` (no network on wasm32 targets).
- **Layer 5 (`photocraft-engine`):** User operations are registered as standard commands (`ai_cmds.rs`), completely callable through GUI, CLI, and MCP automation.
- **Fail-safe:** If inference fails or the server is disconnected, the engine seamlessly uses built-in CPU reference algorithms (`content_aware_native`, `local_blur`).

---

## 3. MVP Milestones

### MVP-1: Local Server Foundation (Current Scope)
- [x] Create `photocraft-ml` crate (L4) with `InferenceBackend` trait.
- [x] Implement `LocalBackend` with Ollama / llama.cpp REST client.
- [x] Implement base64 PNG encode/decode using `photocraft-codecs` and `photocraft-raster`.
- [x] Implement `ClassicalFallback` returning `MlError::Unsupported`.
- [x] Add `AiPrefs` to Edit › Preferences (`localUrl`, `localModel`, `localTimeoutSecs`, `maxRegionMpx`).
- [x] Implement `ai.removeObject` command: selection-masked inpainting with classical PatchMatch fallback.
- [x] Implement `ai.skinEnhance` command: skin detection + frequency separation smoothing with classical fallback.
- [x] Implement `ai.pingBackend` diagnostic command for connectivity checks.

### MVP-2: Interactive UI & On-Canvas Polish
- [ ] On-canvas progress overlay for running inference jobs.
- [ ] Cancel support (Escape key aborts in-flight local inference).
- [ ] Before/After split preview toggle for AI filters.
- [ ] Properties panel options bar controls for AI retouching tools.

### MVP-3: Local Specialized Vision Models
- [ ] Bundled / locally hosted SAM2 (Segment Anything) promptable segmentation.
- [ ] Local Real-ESRGAN / super-resolution upscaler integration.
- [ ] Direct GGUF loading or ONNX Runtime execution via local provider.

### MVP-4: Hosted VLM / Cloud Providers (Deferred)
- [ ] Remote provider abstraction (OpenAI, Stability AI, Fal.ai).
- [ ] API key vault via OS Keychain.
- [ ] Explicit opt-in consent dialog before any pixel data leaves the machine.

---

## 4. Engine Command Specifications

### 4.1 `ai.removeObject`
- **ID:** `ai.removeObject`
- **Menu:** `Edit` › `Content-Aware` › `Remove Object...`
- **Parameters:**
  ```json
  {
    "layer": 1,
    "prompt": "clear background",
    "seed": 42,
    "strength": 0.85,
    "output": "newLayer"
  }
  ```
- **Behavior:**
  1. Validates selection presence and checks region bounds against `maxRegionMpx`.
  2. Encodes active layer region and selection mask to base64 PNG.
  3. Sends request to local server (`POST /api/generate`).
  4. If server unreachable, invokes `content_aware_native` (classical PatchMatch).
  5. Commits result non-destructively as a new raster layer above the active layer (or in-place if `"output": "inPlace"`).

### 4.2 `ai.skinEnhance`
- **ID:** `ai.skinEnhance`
- **Menu:** `Filter` › `Neural Filters` › `Skin Enhance...`
- **Parameters:**
  ```json
  {
    "layer": 1,
    "strength": 50,
    "smoothRadius": 6,
    "preserveDetail": true
  }
  ```
- **Behavior:**
  1. Extracts bounding region of active pixel layer.
  2. Attempts segmentation via local backend; falls back to adaptive HSV skin-luma heuristic mask.
  3. Decomposes image frequencies using Gaussian local blur.
  4. Smooths low-frequency blemishes while keeping high-frequency texture when `preserveDetail` is active.
  5. Applies result back into document with history tracking.

### 4.3 `ai.pingBackend`
- **ID:** `ai.pingBackend`
- **Menu:** (None, diagnostic/preferences only)
- **Parameters:** `{}`
- **Behavior:** Checks local server endpoint (`/api/tags`) and returns connectivity status and latency.

---

## 5. Configuration & Preferences

Preferences stored under `ai`:
- `localUrl` (string, default: `"http://127.0.0.1:11434"`): Local Ollama or llama.cpp endpoint.
- `localModel` (string, default: `"llava"`): Model tag for vision/inpaint tasks.
- `localTimeoutSecs` (u32, default: `120`): Maximum time to wait for local generation.
- `maxRegionMpx` (f32, default: `24.0`): Guard against OOM on massive canvas areas.

