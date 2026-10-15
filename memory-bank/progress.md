# PhotoCraft Progress

## Milestone Status
- **M0 Skeleton:** Done (workspace, CI, xtask layers/wasm/parity).
- **M1 Foundation:** Done (geom, color with 27 blend modes, COW raster, doc, ops, cms).
- **M2 PSD v1:** Done (standalone `photocraft-psd` with high byte-exact round-trip rate).
- **M3 Viewer App:** Done (egui desktop and browser shell, 13+ codecs).
- **M4 Native Format + Engine:** Done (~700 commands, `.pcraft` format with atomic save and crash recovery, CLI).
- **M5 GPU Compositor:** In Progress (wgpu compositor matches CPU <= 1/255; Multichannel fallback to CPU).
- **M6 Paint + Select:** In Progress (brush engine, selection tools, snapping, free transform, pen tablet support).
- **M7 Adjustments & Filters:** In Progress (16 adjustment layers, 70+ filters, Actions record/replay).
- **M8 PSD v2:** In Progress (smart objects, effects, text, shapes; expanding Photoshop oracle test coverage).
- **M9 Text, Vector, Styles:** In Progress (HarfBuzz text, vector shapes, all 10 layer effects on CPU & GPU).
- **M10 Smart Features:** In Progress (classical Select Subject, Content-Aware Fill/Scale, Healing; ML backend not yet started).
- **M11 Automation & Formats:** In Progress (MCP server, batch processing, Image Processor; remaining format codecs pending).
- **M12 Pro Parity:** In Progress (CMYK/Lab/Duotone, soft proofing, channels; timeline and HDR pending).

## Validated Workflows (Alpha Gate)
- Photo retouch (JPEG/RAW open, healing/clone, adjustments, export) - Works end to end.
- Composite (layers, masks, selections, blend modes, smart objects) - Works end to end.
- Cut-out (subject selection, refine edge, masking, transparent PNG) - Partial but non-blocking.
- Design (type, vector shapes, layer effects, artboards, export) - Partial but non-blocking.
- Pen tablet painting (pressure/tilt/rotation handling) - Partial (feel & latency polish needed).
- PSD exchange (import/export with Photoshop) - Partial (blocking for Beta; active focus).

## Known Blockers & Next Objectives
- Target Beta milestone readiness (~75% real-work capability).
- Close Photoshop rejection bugs on exported PSDs.
- Meet interactive performance budgets for brush rendering and heavy layer stacks.

