# PhotoCraft Tech Context

## Core Technologies
- **Language:** Rust 2021 edition.
- **UI Framework:** egui / eframe (immediate mode, native rendering).
- **Graphics API:** wgpu (WGSL shaders) targeting Vulkan, Metal, DirectX 12, and WebGPU.
- **Multithreading:** rayon for parallel CPU tile processing.
- **Color & Profiles:** Proprietary `cms` ICC engine; no external lcms dependency.
- **Automation:** Model Context Protocol (MCP) server, TCP JSON control protocol.
- **AI Inference (M10):** `photocraft-ml` (L4): `InferenceBackend` trait, `LocalBackend` (Ollama / llama.cpp-compatible REST, feature-gated `native`, off on wasm), `ClassicalFallback` (returns `Unsupported`; engine substitutes PatchMatch / HSV+frequency-separation). Commands in `engine/src/ai_cmds.rs`, run as background jobs (`crate::jobs`).
- **Web Build:** WebAssembly (wasm32-unknown-unknown, trunk, wasm-bindgen).

## Technical Constraints & Standards
- **Zero Panic Policy:**
  Non-test code strictly forbids `unwrap()`, `expect()`, `panic!`, `todo!`, `unimplemented!`, and unchecked indexing.
  Enforced by `#![deny(clippy::unwrap_used, ...)]` and automated `panic_hunt` tests.
- **No Unsafe Code:**
  Workspace enforces `unsafe_code = "forbid"`.
  The only exception is `photocraft-tablet/src/macos.rs` (`unsafe_code = "deny"`) for native AppKit tablet event reading.
- **No Webviews or JS Frameworks:**
  No Tauri, Electron, Node.js, or webview wrappers. Desktop app is 100% native Rust binary.
- **Runtime Pixel Formats:**
  Depth (U8, U16, F16, F32) and color model (RGB, Gray, CMYK, Lab) are dynamic properties, never hard-coded `u8` buffers in engine APIs.
- **Atomic File Format Updates:**
  Changes to `photocraft-doc` types must update `photocraft-format` with backward-compatible defaults.

## Development & Tooling
- **Build & Test:**
  `cargo test -p <crate> --lib --tests`
  `cargo clippy -p <crate> --all-targets -- -D warnings`
- **Xtask Suite:**
  - `cargo xtask layers`: Enforces layer boundary rules.
  - `cargo xtask wasm`: Checks WebAssembly compilation for L0-L6.
  - `cargo xtask parity`: Updates menu parity checklist against Photoshop 2026.
  - `cargo xtask scorecard`: Audits performance budgets, corpus pass rates, and preferences.
  - `cargo xtask perf --quick`: Runs hot path performance benchmarks.
  - `cargo xtask test-corpus`: Runs golden oracle tests on real-world PSDs.

## Windows Build Friction (this machine, 2026-10)
- **Defender ASR rule `3b576869-a4ec-4529-8536-b80a7769e899`** ("block executables unless they meet a prevalence, age, or trusted list criterion", action = Block) refuses to execute every freshly compiled `build-script-build.exe` / new unsigned exe: cargo fails with `Zugriff verweigert (os error 5)` ("never executed"), also from cmd/bash. ~1–2 minutes later the cloud verdict arrives and the same exe runs.
- **Workaround:** retry `cargo` every ~90 s — each attempt passes the previously blocked exes and gets one wave further; converges after several rounds. A fresh `CARGO_TARGET_DIR` restarts the cycle from scratch.
- **Permanent fix (needs elevation):** `Add-MpPreference -ExclusionPath 'C:\Temp\photocraft\target'` (ASR honours AV path exclusions).

