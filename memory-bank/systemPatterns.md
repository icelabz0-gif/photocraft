# PhotoCraft System Patterns

## Workspace Layer Architecture
Layering is strictly enforced by `cargo xtask layers`. Higher layers may only depend on lower layers.

- **L0 Foundation:**
  - `geom`: Geometry primitives, affine transforms, bounding boxes.
  - `cms`: ICC profile management, color transforms, 3D LUT generation.
  - `color`: Pixel formats, 27 ISO 32000 blend modes, channel operations.
  - `raster`: Sparse copy-on-write (COW) 256x256 tile store, 64-bit coordinate space.
  - `psd`: Standalone Adobe PSD/PSB parser and writer (publishable, 0 workspace deps).
  - `codecs`: Standalone flat image codecs (JPEG, PNG, TIFF, WebP, etc.).
  - `tablet`: Native pen tablet input handling (isolated crate with macOS AppKit event monitor).
- **L1 Document Model:**
  - `doc`: Document structure, layer tree, masks, adjustments, effects, smart objects.
- **L2 Operations & Engines:**
  - `ops`: Undo/redo history, command transactions, document mutation pipelines.
  - `paint`: Brush engine, dab generators, stroke smoothing, dynamics.
  - `algo`: Image processing algorithms (blur, sharpening, warp, content-aware, color adjustments).
  - `text`: HarfBuzz/FreeType text shaping, typography engine, font management.
  - `vector`: Paths, Bézier curves, shape rasterization, stroke geometry.
- **L3 Compositing & Storage:**
  - `compose`: Reference CPU compositor (the oracle) using rayon parallel tiles.
  - `gpu`: GPU canvas compositor (wgpu / WGSL, pixel parity <= 1/255 vs CPU).
  - `format`: Native `.pcraft` atomic file storage, crash recovery.
- **L4 I/O & Extensions:**
  - `io`: Document translation between `doc` and external formats (PSD, TIFF, PDF).
  - `plugins`: Sandboxed WebAssembly plug-in host environment.
- **L5 Application Engine:**
  - `engine`: Headless session management, command registry (`CommandSpec`), parameter schemas.
- **L6 Presentation & Automation:**
  - `ui-egui`: Pure Rust egui shell, panels, canvas view, theme tokens (`Tokens`).
  - `automation`: MCP server and JSON TCP control protocol handlers.
- **Applications (`apps/`):**
  - `photocraft`: Desktop executable (eframe/wgpu).
  - `photocraft-cli`: Headless CLI runner (convert, batch, info, mcp).
  - `photocraft-web` & `photocraft-web-boot`: Browser wasm target.

## Key Design Patterns & Invariants
1. **Command Pattern:**
   Every user-visible operation is registered with a unique string `CommandId` in `engine`. The UI, CLI, MCP server, and tests invoke the same commands.
2. **Sparse Copy-on-Write Tiles:**
   Images are stored as 256x256 pixel tiles wrapped in `Arc`. Snapshotting is O(layers) and lock-free, making undo and background caching lightweight.
3. **Dual CPU/GPU Algorithm Parity:**
   Algorithms have a deterministic CPU reference (rayon) and an optional GPU WGSL shader. Both consume identical `#[repr(C)] bytemuck::Pod` parameter structs.
4. **Thin, Data-Driven UI:**
   UI state is decoupled from core logic and stored in serde-serializable structs (`ui-egui/src/state.rs`). Controls and dialogs are generated from engine schemas.
5. **Clean Seam & Zero Webviews:**
   Core logic never depends on egui, winit, or rfd. No JS or webviews are allowed anywhere in the architecture.

