# Agent Guidance

This repository contains `yash-app-events`, a Linux-first application that captures game frames, detects configured HUD states and transitions, and exposes those observations as durable files and live IPC events.

## Required reading order

Before changing code or documentation, read these files in order:

1. `SPECS.md` — normative product and technical requirements; this is the source of truth.
2. `PLAN.md` — implementation sequence, milestones, and verification gates.
3. `GOAL.md` — autonomous execution objective and stopping conditions.
4. `README.md` — user-facing description, setup, and currently supported behavior.

If these documents disagree, `SPECS.md` wins. Resolve the disagreement in the same change rather than allowing documentation to remain contradictory.

## Working rules

- Target Linux and Wayland first. Treat Windows support as a future capture backend, not a current requirement.
- Keep capture, detection, event rules, persistence, output, IPC, CLI, and GUI behind explicit boundaries.
- The daemon is the sole owner of capture sessions, active detector state, profile writes, and output writes.
- The GUI and CLI must use the same versioned control protocol. Do not create GUI-only control paths.
- Never perform capture, OCR, model inference, or filesystem writes on the GUI render thread.
- Use a latest-frame policy. Never allow unbounded frame queues or process stale frames merely because they were captured.
- Store regions as normalized coordinates and retain reference resolution/aspect-ratio metadata.
- Treat detector observations and emitted events as different concepts. Events must be produced by temporal rules/state transitions.
- Configuration writes and current-state writes must be atomic.
- Keep portable profile data separate from machine-local portal restore tokens and capture bindings.
- Use stable IDs in schemas and external output. Display names may change without breaking integrations.
- Do not add a neural model where deterministic image processing is sufficient. Measure first.
- Do not claim support in `README.md` until it is implemented and verified.

## Change discipline

For each implementation task:

1. Identify the relevant specification IDs in `SPECS.md`.
2. Add or update tests that demonstrate the required behavior.
3. Implement the smallest coherent vertical slice.
4. Run formatting, linting, unit tests, and applicable integration tests.
5. Update the status and evidence in `SPECS.md`.
6. Update `PLAN.md` if sequencing or scope changed.
7. Update `README.md` only for user-visible behavior that now works.

Do not mark a requirement complete based only on code presence. Completion requires the verification evidence stated in `SPECS.md` or `PLAN.md`.

## Intended workspace shape

The detailed crate layout is specified in `SPECS.md`. Prefer a Cargo workspace with small crates for protocol, profile schemas, capture, vision, engine, output, daemon, CLI, and GUI. Shared public types belong in the narrowest appropriate crate; avoid a generic catch-all `common` crate.

## Expected quality commands

Once the Cargo workspace exists, the minimum local verification is:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Add targeted integration, replay, and schema-compatibility commands as those facilities are implemented. Record canonical commands in `README.md`.

## Safety and privacy

- Create the control socket under `XDG_RUNTIME_DIR` with user-only access.
- Do not expose TCP or WebSocket control by default.
- Validate imported archives, paths, sizes, schemas, templates, and models before installation.
- Do not export portal restore tokens, local window identifiers, or other machine-specific capture data.
- Never execute content from imported profiles.
- Ensure debug captures are opt-in and make their storage and deletion visible to the user.

