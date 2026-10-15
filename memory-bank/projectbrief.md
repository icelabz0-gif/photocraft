# PhotoCraft Project Brief

## Overview
PhotoCraft is an open-source, native, Photoshop-comparable layered image editor written in 100% Rust. It targets desktop (macOS, Windows, Linux) via egui/eframe on wgpu, and browser via WebAssembly (trunk + wasm-bindgen). No webview shells, Tauri, Electron, or JavaScript UI frameworks.

## Core Goals
- **Photoshop Parity:** 1:1 feature, menu, shortcut, and behavioral parity with Adobe Photoshop 2026.
- **Superior Performance:** Outperform traditional editors via sparse COW tile architecture and multithreaded/GPU pipelines.
- **Headless & Agent-First:** Every feature is reachable headlessly via CLI, JSON control protocol, and Model Context Protocol (MCP).
- **Clean-Room Engineering:** Built strictly from public specifications, papers, and behavioral observation. No proprietary code copied.
- **Rock-Solid Stability:** Zero panic policy. Hostile input handling, bounds checks, and resilient error propagation.

## Scope & Target
- **Target App:** Adobe Photoshop 2026 (27.11.0).
- **Current Stage:** Alpha (v0.6.0, ~45% ready for real work, 76% feature breadth).
- **Next Milestone:** Beta (~75% readiness, reliable two-way PSD interchange).

