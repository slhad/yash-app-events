# Detailed Implementation Plan

This plan sequences the requirements in `SPECS.md` into verifiable vertical slices. `SPECS.md` remains authoritative if scope or behavior differs.

## Progress

- Phase 0: complete (2026-07-11). Workspace formatting, strict Clippy, tests,
  documentation, and README claim checks pass locally; see the requirements evidence
  index in `SPECS.md`.
- Phase 1: complete (2026-07-11). Profile schemas, atomic local storage, revisions,
  duplication, trash/restore, migration dispatch, and resource-limited portable
  archives pass sixteen profile tests.
- Phase 2: complete (2026-07-11). Protocol-v1 Unix transport, concurrent clients,
  profile lifecycle RPCs, bounded subscriptions, and the real CLI pass daemon-backed
  integration and golden-output tests.
- Phase 3: complete (2026-07-11). Synthetic replay frames traverse color detection,
  typed observations, temporal rules, daemon-owned JSONL/atomic state output, and a
  bounded live RPC subscription in one end-to-end integration test.
- Phase 4: complete (2026-07-11). Color-bar, multi-template, and region-change
  detectors use serializable preprocessing, bounded diagnostic PNG previews, portable
  assets, profile replay, daemon RPC, temporal output, and recorded benchmarks.
  `docs/vision.md` records why measured pure-Rust routines defer the proposed OpenCV
  dependency while retaining replaceable project-owned boundaries.
- Phase 5: complete (2026-07-11). Hyprland create/select/start/open-remote/frame/stop,
  restore-token reuse, isolated fresh-permission cancellation, typed policy denial,
  formats, metrics, and bounded latest-frame behavior pass. Other desktops are deferred.
- Phase 6: complete (2026-07-11). Installed native GUI source selection, bounded
  high-detail preview/freeze, normalized region persistence, detector diagnostics,
  live metrics/evidence, and daemon-late reconnect recovery pass on Hyprland.
- Phase 7: complete (2026-07-11). Detector/rule forms, frozen and continuous testing,
  diagnostic previews, explicit template capture, temporal state, and the bounded
  observation/transition timeline complete the no-code deterministic authoring path.
- Phase 8: complete (2026-07-11). Versioned synthetic manifests, common-path daemon evaluation,
  event metrics, regression thresholds, CLI JSON/exit status 7, and GUI import,
  playback/event scrubbing and metrics satisfy deterministic tuning and regression gates.
  Profile replay also publishes through durable state/events and enabled output routes;
  long image/OCR GUI requests have a bounded five-minute completion window (2026-07-17).
- Phase 9: complete as post-release work (2026-07-11). Synthetic English,
  localization, scale, animation, and glow fixtures; reproducible accuracy,
  confidence, latency, CPU, and memory comparison; Tesseract backend decision;
  typed detector; change-triggered scheduling; profile/daemon/GUI configuration;
  frozen diagnostics; and common-path image replay events pass.
- Phase 10: complete (2026-07-11). A clean-prefix source install, user service, desktop
  metadata, icon, Bash completion, man pages, recovery documentation, security review,
  Hyprland portal acceptance, and installed GUI workflow pass.

## Planning principles

- Build one end-to-end path early, then deepen individual subsystems.
- Keep the daemon usable without the GUI so capture and detection can be tested independently.
- Introduce external schemas deliberately and test them before relying on them.
- Defer OCR, ONNX, DMA-BUF, and custom OBS integration until deterministic detection works.
- Each phase ends with a runnable demonstration and recorded verification evidence.

## Phase 0 — Repository and engineering baseline

Goal: establish a reproducible Rust workspace and quality gates.

Steps:

1. Initialize Git with a main branch and appropriate Rust/Linux `.gitignore`.
2. Create the Cargo workspace and logical crates described by `SPEC-ARCH-002`.
3. Pin the minimum supported Rust version and document it.
4. Add workspace-wide dependency, lint, formatting, and unsafe-code policies.
5. Add `tracing`-based structured logging infrastructure.
6. Add error conventions: typed library errors and contextual application errors.
7. Add CI for formatting, Clippy with warnings denied, unit tests, and documentation checks.
8. Add a command or test that verifies `README.md` does not claim unimplemented release features where practical.
9. Record canonical developer commands in `README.md`.

Exit gate:

- Workspace builds on Linux.
- `cargo fmt`, `cargo clippy`, and `cargo test` pass.
- CI runs the same checks.
- Crate dependency direction is documented.

## Phase 1 — Versioned schemas and local storage

Goal: make configuration safe before capture or UI depends on it.

Requirements: `SPEC-PROFILE-001` through `SPEC-PROFILE-011`, relevant security requirements.

Steps:

1. Define strongly typed IDs for profiles, elements, detectors, rules, and daemon instances.
2. Define profile schema version 1 with game metadata, layout metadata, normalized regions, detector definitions, and event rules.
3. Define validation errors with paths suitable for GUI display.
4. Implement XDG path resolution with deterministic test overrides.
5. Implement atomic file replacement and failure-injection tests.
6. Implement profile create, load, validate, save, list, rename, and activate operations.
7. Implement drafts and bounded revision history.
8. Implement optimistic revisions on mutation.
9. Implement deep profile and element duplication semantics.
10. Implement application trash and restore.
11. Define the export manifest and `.hudprofile` archive format.
12. Implement safe export/import with traversal, symlink, expansion, and size protections.
13. Add golden schema fixtures and round-trip tests.
14. Create migration infrastructure even though version 1 has no prior migration.

Exit gate:

- Profiles survive round trips without semantic changes.
- Interrupted saves retain the previous valid profile.
- Duplicate profiles share no mutable identity or capture binding.
- Malicious archive fixtures are rejected.
- Schema fixtures are reviewed and treated as external contracts.

## Phase 2 — JSON-RPC daemon skeleton and CLI

Goal: establish the control plane before adding capture.

Requirements: `SPEC-ARCH-001`, `SPEC-IPC-001` through `SPEC-IPC-006`, `SPEC-SEC-001`.

Steps:

1. Define protocol version 1 request, response, error, notification, and handshake types.
2. Decide whether to use `jsonrpsee` services or a narrow Tokio Unix-socket server; document the decision.
3. Implement newline-framed compact JSON with message and nesting limits.
4. Create the socket below `XDG_RUNTIME_DIR` with mode `0600`.
5. Enforce one daemon instance with safe stale-socket recovery.
6. Implement handshake, version, capabilities, status, and graceful shutdown methods.
7. Expose profile operations from Phase 1 over RPC.
8. Implement bounded event/status subscriptions and lag behavior.
9. Build `yash-eventsctl` as a real RPC client.
10. Add `--json`, stable exit codes, timeouts, and actionable connection errors.
11. Add offline profile validation using the shared profile crate.
12. Add protocol integration tests with concurrent and deliberately slow clients.

Exit gate:

- The daemon starts and stops cleanly.
- Two clients can concurrently inspect state.
- A stale profile update receives a structured revision conflict.
- A slow subscriber cannot increase daemon memory without bound.
- CLI JSON output has golden tests.

## Phase 3 — Replay-first frame and engine foundations

Goal: prove scheduling, observations, rules, and output without portal variability.

Requirements: `SPEC-ARCH-003`, `SPEC-ARCH-004`, `SPEC-DET-001`, `SPEC-EVENT-*`, `SPEC-OUT-*`, `SPEC-REPLAY-*`.

Steps:

1. Define backend-neutral frame, region view, pixel-format, detector, observation, rule, transition, and sink interfaces.
2. Implement a synthetic/image-sequence replay capture backend with explicit timestamps.
3. Implement the latest-frame slot and configurable analysis scheduler.
4. Implement crop validation and normalized-to-pixel coordinate conversion.
5. Implement typed observation histories.
6. Implement numeric threshold, confidence threshold, N-of-M, hysteresis, and cooldown rules.
7. Define JSONL event schema version 1 and current-state schema version 1.
8. Implement append-only event output and atomic state snapshots.
9. Surface sink failures without terminating the engine.
10. Add daemon instance IDs and monotonically increasing event sequence numbers.
11. Build deterministic synthetic replay fixtures.
12. Verify identical input frames/timestamps yield identical output.

Vertical-slice demonstration:

- Replay synthetic health-bar frames.
- Produce numeric health observations.
- Emit one `critical_health entered` event and one `left` event.
- Observe both through JSONL/state files and an RPC subscription.

Exit gate:

- Deliberately slow processing does not create a stale-frame backlog.
- Event transition golden files pass.
- Output interruption/failure behavior is tested.

## Phase 4 — Deterministic vision detectors

Goal: provide useful HUD detection without OCR.

Requirements: `SPEC-DET-002`, `SPEC-DET-003`, `SPEC-DET-004`, `SPEC-DET-007`.

Steps:

1. Integrate OpenCV behind the vision crate rather than exposing OpenCV types across the workspace.
2. Implement explicit image conversion and preprocessing pipelines.
3. Implement color/range bar measurement with debug masks and fill diagnostics.
4. Implement normalized template matching with multiple templates and optional masks.
5. Implement region-change and stability metrics.
6. Implement detector configuration validation.
7. Add detector-test RPC methods returning observations and bounded diagnostic images/metadata.
8. Create synthetic fixtures for scale, minor noise, brightness variation, and partial bars.
9. Benchmark processing time per detector and record a baseline.

Exit gate:

- All three detectors work through replay, daemon, RPC, and output.
- Preprocessed/debug views can be requested without enabling persistent capture.
- Detector failures produce `unknown` or `error`, never false negatives.

## Phase 5 — Wayland portal and PipeWire capture

Goal: replace replay input with a real Linux capture source.

Requirements: `SPEC-CAP-001` through `SPEC-CAP-006`, `SPEC-PROD-002`, `SPEC-PROD-004`.

Steps:

1. Build a small capture spike using `ashpd` to create, select, start, and close a ScreenCast session.
2. Record tested portal backend, compositor, PipeWire, and negotiated format versions.
3. Consume the portal-provided PipeWire remote and node.
4. Decide direct PipeWire versus GStreamer consumption based on spike complexity and measured copies; document the decision.
5. Support one packed RGB format end-to-end and emit clear unsupported-format diagnostics.
6. Exclude the cursor by default when supported.
7. Connect the capture callback to the latest-frame slot without blocking it.
8. Implement configurable 1–10 FPS analysis throttling.
9. Handle portal cancellation, permission denial, stream closure, source resize, and daemon shutdown.
10. Store and reuse portal restore tokens when supported, with source-picker fallback.
11. Expose capture metrics and lifecycle through RPC and CLI.
12. Add an opt-in snapshot command.
13. Write a manual smoke-test procedure for at least two available Wayland portal environments when accessible.

Exit gate:

- A selected game or test window produces observations at 10 analysis FPS.
- A 60 FPS producer does not cause detector backlog.
- Capture stops promptly and releases portal/PipeWire resources.
- No frame is persisted without explicit user action.

## Phase 6 — GUI foundation and live preview

Goal: provide visual source selection and region setup.

Requirements: `SPEC-UI-001` through `SPEC-UI-004`, `SPEC-UI-008`, `SPEC-CAP-005`.

Steps:

1. Create the `eframe`/`egui` application as an IPC client.
2. Implement connection lifecycle, compatibility handshake, reconnect, and daemon error surfaces.
3. Implement profile manager: create, rename, duplicate, activate, import, export, trash, and restore.
4. Implement capture source selection and live metrics.
5. Implement bounded preview transport; begin with compressed preview if necessary.
6. Ensure preview is opt-in and stops when no client needs it.
7. Implement freeze-frame mode.
8. Build the zoomable/pannable canvas.
9. Implement drawing, selection, movement, resizing, duplication, labels, colors, and enable state for normalized regions.
10. Display reference-pixel and normalized coordinates.
11. Add draft autosave, unsaved-state indication, validation display, save, revert, and revision-conflict handling.
12. Verify no blocking work occurs on the render thread.

Exit gate:

- A user can select a live source, freeze it, draw a region, save it, restart, and load the same region.
- Duplicate profile/element operations are accessible and correct.
- The UI remains responsive while capture and synthetic slow detectors run.

## Phase 7 — Detector and event authoring UI

Goal: complete the no-code configuration workflow.

Requirements: `SPEC-UI-005` through `SPEC-UI-007`.

Steps:

1. Implement detector type selection and detector-specific forms.
2. Show original crop, processed crop/mask, current value, confidence, and diagnostics.
3. Implement test-on-frozen-frame and continuous-test modes.
4. Implement event-rule forms for the first rule primitives.
5. Visualize N-of-M history, enter/leave thresholds, hysteresis, cooldown, and current state.
6. Add recent observation and transition timeline.
7. Add explicit snapshot/debug-crop save actions with destination disclosure.
8. Add validation preventing activation of broken rules or missing assets.
9. Add contextual help explaining region, detector, observation, rule, and event concepts.

Exit gate:

- A user can build the complete synthetic health example without editing YAML.
- A user can capture and configure a template detector from the UI.
- Live transitions appear in the UI, JSONL, state file, CLI, and RPC subscription.

## Phase 8 — Replay workflow and evaluation

Goal: make tuning reproducible and measurable.

Requirements: `SPEC-REPLAY-001` through `SPEC-REPLAY-006`.

Steps:

1. Define a replay manifest containing frame/video source, timing, expected events, and optional annotations.
2. Add explicit user-controlled replay recording or import.
3. Route replay through the identical detector, derived-observation, event, durable
   publication, and enabled output-route boundaries.
4. Add frame scrubbing and playback controls to the GUI.
5. Overlay regions, observations, rules, and events during replay.
6. Implement expected-event annotations.
7. Report precision, recall, duplicates, misses, and detection latency.
8. Add CLI replay evaluation with JSON output and nonzero exit status for configured regressions.
9. Create CI-safe synthetic replay suites.
10. Support ignored or separately distributed real-game regression packages containing
    a pinned profile, checksummed media, full/partial/zone-crop placement, typed
    observation expectations, event expectations, and categorized JSON results through
    the shared daemon/CLI protocol.
11. Add an opt-in daemon-owned passive evidence collector with a machine-local 70-second
    default policy, bounded jitter/quotas, perceptual plus evidence-aware deduplication,
    exact-frame metadata, and a shared GUI/CLI/JSON-RPC review queue. Support
    accept/correct/reject, conservative automatic batch review, and checksum-updating
    promotion into the external package.

Exit gate:

- Detector tuning can be performed without a running game.
- A known regression changes a replay metric and fails its test.
- Live and replay processing share the same engine path.
- The local BlazBlue package evaluates 33 categorized cases / 50 frames / 126 typed
  observation assertions without installing the pinned profile; 12 are reviewed passive
  captures. A real low-motion 10-second pair is rejected as similar at 0.008501 versus
  the configured 0.015 threshold (2026-07-14).

## Phase 9 — OCR spike and integration

Goal: add text recognition based on measured game-HUD needs.

Requirements: `SPEC-DET-005`.

Steps:

1. Collect or synthesize legally redistributable representative HUD text crops.
2. Define benchmark metrics: correct field/event result, latency, confidence calibration, and CPU/memory cost.
3. Spike Tesseract through a Rust binding.
4. Spike one ONNX recognition pipeline through ONNX Runtime.
5. Compare accuracy after explicit preprocessing.
6. Select one initial backend or document why both are justified.
7. Implement bounded OCR scheduling triggered by region change where appropriate.
8. Add OCR configuration and preprocessing preview to the GUI.
9. Add replay regression cases for animation, glow, scaling, and localization.

Exit gate:

- Backend choice is supported by recorded benchmark results.
- OCR does not block capture or GUI rendering.
- OCR-derived events obey the same temporal-rule path as all other detectors.

## Phase 10 — Packaging and first usable release

Goal: deliver an installable Linux application with stable initial contracts.

Steps:

1. Finalize binary and application IDs.
2. Freeze protocol version 1 and profile/output schema version 1.
3. Add shell completions and man pages for the CLI.
4. Add systemd user service and optional socket activation.
5. Package desktop entry, icon, and portal-facing application metadata.
6. Choose initial distribution formats based on tested dependency handling.
7. Document supported distributions/desktops and known portal limitations.
8. Add upgrade, backup, export, recovery, and uninstall documentation.
9. Conduct security review of IPC, imports, paths, logs, and debug images.
10. Conduct performance profiling on documented reference hardware.
11. Run clean-machine installation and end-to-end acceptance tests.
12. Update `README.md` from planned language to verified usage only.

Exit gate:

- A new user can install, select a source, configure a detector/event, restart, and consume the event through documented outputs.
- Schemas and CLI behavior have golden compatibility tests.
- All release-scoped specification requirements have evidence and `VERIFIED` status.

## Phase 11 — Post-release work

Completed after the first usable Linux release (2026-07-11):

- Complete event-rule language (boolean/string/stable-duration/composition and
  configurable updated/initial transitions).
- OCR detector integration from Phase 9.
- Privacy-bounded diagnostic bundle.
- Generic ONNX image classifier workflow.
- Profile-scoped output routing with JSON/raw-text templates, file/direct-command sinks, bounded
  daemon execution, shared CLI/RPC control, and GUI enable/test verification.
- Portable inert output recipes with archive validation, provenance/hash review,
  side-effect-free GUI preview/editing, explicit local sink selection, and disabled install.
- Single-release public Profile Catalog with immutable versioned packages, append-only indexes,
  daemon-owned bounded download/cache/import, shared CLI/RPC control, and collapsed GUI review.

Remaining candidates are deliberately unscheduled:

- Shared-memory preview optimization.
- DMA-BUF/GPU preprocessing.
- OBS plugin/shared-texture integration.
- X11 backend.
- Windows Graphics Capture backend.
- Authenticated remote WebSocket control.
- MQTT, Home Assistant, and webhook output adapters.
- Multi-source simultaneous capture.

Promote a remaining candidate only after a concrete use case, benchmark, and specification update.
Milestone ordering and exit gates are maintained in `ROADMAP.md`.

## Cross-phase verification checklist

At every phase boundary:

1. Run formatting, Clippy, all tests, and applicable smoke tests.
2. Confirm bounded queues and cancellation behavior.
3. Run schema/protocol golden tests after external-contract changes.
4. Review logs and fixtures for portal tokens, private screenshots, and absolute local paths.
5. Update `SPECS.md` status/evidence entries.
6. Update this plan if sequencing changed.
7. Update `README.md` only for behavior now verified.
8. Leave the repository in a buildable, testable state.

## Definition of first usable product

The first usable product is complete when a Linux Wayland user can:

1. Install and start the daemon and GUI.
2. Select a window through the ScreenCast portal.
3. See a bounded live preview.
4. Draw and persist a normalized HUD region.
5. Configure a color-bar or template detector.
6. Configure a debounced event rule.
7. Validate it live and in replay.
8. Observe exactly-on-transition output in `events.jsonl`, `state.json`, CLI, and IPC.
9. Duplicate, export, import, recover, and migrate the configuration safely.
10. Restart the application without losing the profile or corrupting output.
