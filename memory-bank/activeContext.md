# PhotoCraft Active Context

## Current Status
- **Version:** 0.6.0 (Alpha stage).
- **Readiness:** ~45% ready for real work (Alpha gate passed for all 6 core daily workflows).
- **Parity Score:** Menu wiring 628/628 (100%), feature breadth ~76%, 56/68 tools, 30/35 panels.

## Current Priorities (from Roadmap & Gaps)
1. **G1: PSD Acceptance by Photoshop:**
   - Fix PSD exporter issues where Photoshop rejects generated files (#1281, #2469).
   - Improve Photoshop oracle rendering floor from 133/256 towards 200+.
2. **G2: Performance Budgets:**
   - Address the 22 over-budget performance scenarios in `perf/budgets.toml`.
   - Resolve P12 GPU crash at 150 layers and brush/cursor latency (#2619, #2566).
3. **G3: Bug Backlog Triage:**
   - Resolve high-priority regressions and user-reported bugs from 0.5.x/0.6.0 releases.
4. **G4 & G5: Brush & Transform Feel:**
   - Bring brush dynamics, stroke interpolation, transform handles, and modifier behaviors up to 1:1 Photoshop feel.

## Active Rules & Development Notes
- Independent target directories per agent (`CARGO_TARGET_DIR=target/agent-<name>`).
- All changes must pass `cargo xtask layers` and maintain wasm compatibility.
- Ensure all command inputs are treated as untrusted and never cause panics.

