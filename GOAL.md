# Autonomous Implementation Goal

## Objective

Implement the Linux-first `yash-app-events` product described in `SPECS.md`, following the sequencing and verification gates in `PLAN.md`, until the definition of the first usable product is genuinely satisfied.

The completed product must let a Wayland user select a game window through XDG Desktop Portal/PipeWire, visually configure HUD regions and deterministic detectors, turn observations into debounced events, persist and duplicate portable profiles, replay inputs for testing, and consume state through durable JSON files, a CLI, and local JSON-RPC IPC.

## Authority and document priority

Read and obey `AGENTS.md` before implementation.

Document priority is:

1. `SPECS.md` — product and technical truth.
2. `PLAN.md` — implementation order and phase gates.
3. `GOAL.md` — autonomous operating loop and completion contract.
4. `README.md` — user-facing verified behavior.

If documents conflict, correct the lower-priority document in the same change.

## Autonomous operating loop

Repeat the following until the completion conditions are met:

1. Inspect repository status and all relevant documentation.
2. Find the earliest incomplete phase in `PLAN.md` whose prerequisites are satisfied.
3. Select the smallest coherent vertical slice that advances its exit gate.
4. Identify the associated `SPEC-*` requirements.
5. Inspect existing code and tests before editing.
6. Implement the slice across all necessary layers rather than leaving dead scaffolding.
7. Add unit, integration, golden, replay, or smoke tests proportional to the change.
8. Run formatting, Clippy, workspace tests, and targeted verification.
9. Fix failures and regressions before proceeding.
10. Update specification status/evidence and plan progress accurately.
11. Update `README.md` only if user-visible behavior is now verified.
12. Commit a coherent checkpoint when repository policy and available credentials allow it.
13. Continue to the next safe slice without waiting for routine confirmation.

## Decision rules

- Make reasonable, reversible implementation decisions autonomously when they remain within `SPECS.md`.
- Prefer a working vertical slice over broad scaffolding.
- Prefer safe maintained libraries, but hide replaceable dependencies behind project-owned interfaces.
- Prefer deterministic vision before OCR or neural inference.
- Prefer replayable tests over manual-only claims.
- Preserve schema and protocol compatibility once released.
- Profile before introducing shared memory, DMA-BUF, GPU preprocessing, or custom OBS integration.
- Do not expand into Windows, remote network control, input automation, or unrelated integrations before the Linux first-usable-product definition is met.
- Do not weaken validation, security, durability, or bounded-queue requirements to make a phase appear complete.
- Do not mark requirements verified without concrete evidence.

## When user input is required

Continue autonomously through ordinary engineering choices. Stop and request input only when:

- two viable choices produce materially different product behavior not resolved by `SPECS.md`;
- an irreversible action or external publication is required;
- necessary credentials, hardware, portal interaction, or copyrighted fixtures are unavailable;
- a requested dependency or license would materially change distribution constraints;
- proceeding would require broadening the authorized scope.

When blocked on hardware or an interactive Wayland test, complete all safe automated work, create an exact reproducible smoke-test procedure, record the missing evidence, and continue with other independent requirements where possible.

## Quality gates

At minimum, keep these passing once the workspace exists:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Also run all applicable schema compatibility, protocol golden, replay regression, import-safety, and Linux capture smoke tests defined during implementation.

## Completion conditions

The goal is complete only when all of the following are true:

- Every requirement needed by the `PLAN.md` definition of first usable product is marked `VERIFIED` in `SPECS.md` with evidence.
- All Phase 0 through Phase 8 exit gates are satisfied.
- Phase 9 OCR may remain deferred if the first usable workflow is complete without it; any claim of OCR support requires its full gate.
- Phase 10 packaging and first-release gates are satisfied for at least one documented Linux installation path.
- The daemon, GUI, and CLI operate through the shared versioned protocol.
- Capture and subscription paths are bounded and remain responsive under slow consumers.
- Profile saves and current-state writes are atomic and recovery-tested.
- Profile duplicate/import/export/migration behavior is tested.
- Live and replay inputs share the same detector and event engine.
- End-to-end tests demonstrate region setup, detection, temporal transition, and all required outputs.
- A clean installation following `README.md` succeeds on a documented supported environment.
- Formatting, linting, tests, and release checks pass from a clean working tree.
- `README.md`, `PLAN.md`, `SPECS.md`, and actual behavior agree.

Do not declare completion merely because code has been written or a subset of tests passes. Completion means the product is installable, usable, documented, and verified against the stated contracts.

## Final handoff

When complete, provide a concise report containing:

- implemented capabilities;
- supported Linux environments and installation method;
- verification commands and results;
- remaining deferred items;
- schema/protocol versions;
- known limitations and recovery procedures.

