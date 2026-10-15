# PhotoCraft Product Context

## Purpose & Need
Professional image editing has long been dominated by closed, subscription-only software (Adobe Photoshop) or open-source editors limited by architectural debt (GIMP historically struggling with 16/32-bit depth, CMYK, non-destructive workflows, and modern UI). PhotoCraft provides a libre, high-performance, native alternative without historical legacy compromises.

## Problems Solved
- **Vendor Lock-in & Pricing:** True open-source layered image editing with native `.pcraft` and full PSD/PSB compatibility.
- **Architectural Debt Avoidance:** Runtime bit depths (8/16/32f) and dynamic color models (RGB, Gray, CMYK, Lab) from day one.
- **Agent Integration:** Direct scriptability and programmatic control via MCP server and JSON control channel, enabling autonomous workflows.
- **Portability:** Fast startup on macOS, Windows, Linux, and WebAssembly browser targets from a single Rust codebase.

## User Experience Goals
- **Familiarity:** Immediate muscle-memory transfer for Photoshop users (identical menus, tool shortcuts, modifier keys, panel docks).
- **Fluidity & Responsiveness:** Interactive 60fps canvas operations, sub-second complex filter execution, zero brush lag.
- **Data Safety:** Atomic saves, undo history backed by immutable COW tile snapshots, crash recovery, and never-panic runtime.

