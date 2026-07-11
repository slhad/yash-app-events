# yash-app-events Specifications

This document is the normative source of truth for product behavior, architecture, external contracts, and acceptance criteria.

## 1. Document conventions

Requirement status values:

- `PLANNED`: accepted requirement with no verified implementation.
- `IN_PROGRESS`: implementation exists but acceptance evidence is incomplete.
- `VERIFIED`: acceptance criteria pass and evidence is recorded.
- `DEFERRED`: explicitly outside the current implementation goal.
- `REJECTED`: considered and intentionally excluded, with rationale.

Every requirement has a stable ID. Code, tests, issues, and plan tasks should reference these IDs.

The initial status of every requirement in this document is `PLANNED` unless stated otherwise.

## 2. Product definition

### SPEC-PROD-001 — Purpose

The system shall allow a user to visually configure detection of meaningful game HUD observations and transitions without relying on a native game API or reading game process memory.

Acceptance:

- A user can select a capture source, define at least one region, configure a detector and event rule, observe a live result, and persist the profile.
- A resulting state transition appears through both durable file output and live IPC.

### SPEC-PROD-002 — Linux-first scope

The first supported platform shall be Linux on Wayland using XDG Desktop Portal ScreenCast and PipeWire.

Acceptance:

- A portal-selected monitor or window produces frames on a supported Wayland desktop.
- Denied and cancelled permission requests produce actionable errors.

Windows capture is `DEFERRED` until the Linux milestones are complete. X11 capture is an optional compatibility backend and is not part of the first usable release.

### SPEC-PROD-003 — Non-invasive operation

The system shall not inject into games, read game memory, automate game input, or attempt to bypass anti-cheat controls.

### SPEC-PROD-004 — Primary analysis rate

The engine shall support a configurable analysis rate from 1 through 10 FPS, defaulting to 10 FPS, independently of the incoming capture rate.

Acceptance:

- A 60 FPS input does not cause more detector evaluations than configured.
- Stale frames do not accumulate.

## 3. System architecture

### SPEC-ARCH-001 — Process model

The system shall consist of one state-owning daemon plus separate GUI and CLI clients.

- The daemon owns capture sessions, active profiles, detector runtime state, profile writes, output writes, and IPC.
- The GUI and CLI use the same versioned IPC protocol.
- Multiple clients may connect concurrently.

### SPEC-ARCH-002 — Cargo workspace

The implementation should use a Cargo workspace with these logical boundaries:

```text
crates/
  protocol/     versioned RPC and external data types
  profile/      profile schema, validation, migration, import/export
  capture/      backend trait and frame types
  capture-pw/   portal and PipeWire implementation
  vision/       detector traits and deterministic detectors
  engine/       scheduler, observations, rules, state machine
  output/       JSONL and atomic state writers
  daemon/       ownership, orchestration, IPC server
  cli/          CLI client
  gui/          egui client and editor
apps/           optional thin binary entry points
```

Exact crate names may change before publication, but the boundaries must remain explicit.

### SPEC-ARCH-003 — Bounded data flow

Capture shall feed a replaceable latest-frame slot or bounded channel. Slow detection must not block PipeWire callbacks or create an unbounded queue.

Acceptance:

- A test that deliberately slows detection demonstrates bounded memory use.
- The detector resumes from the latest frame rather than draining stale frames.

### SPEC-ARCH-004 — Time model

Internal durations and ordering shall use a monotonic clock. External timestamps shall use UTC RFC 3339. Every emitted event shall include a daemon-local monotonically increasing sequence number.

## 4. Capture

### SPEC-CAP-001 — Capture abstraction

Capture backends shall expose frames through a backend-neutral interface carrying at least:

- monotonic timestamp;
- width and height;
- row stride;
- pixel format;
- memory representation;
- source identity when available.

### SPEC-CAP-002 — Portal session

The Linux backend shall use `ashpd` or an equivalent maintained Rust binding to:

- create a ScreenCast session;
- allow selection of a monitor or window;
- request cursor exclusion by default;
- open the PipeWire remote;
- expose cancellation, denial, and closure events.

### SPEC-CAP-003 — Restore tokens

When supported by the portal backend, the application shall store a machine-local restore token and attempt restoration on future sessions. Failure shall fall back to explicit source selection.

Restore tokens shall never be included in exported profiles.

### SPEC-CAP-004 — Pixel formats

The first backend shall support at least one common packed RGB format. Additional negotiated formats shall fail with a clear diagnostic until supported. Format conversion shall be explicit and measured.

### SPEC-CAP-005 — Preview

The daemon shall provide an opt-in preview stream to the GUI. Preview transport must not alter the raw detector input.

The initial implementation may use bounded compressed preview frames. Shared-memory preview is preferred once profiling justifies it. Full-resolution raw frames shall not be embedded in JSON-RPC messages.

### SPEC-CAP-006 — Frame metrics

Status shall expose input FPS, analysis FPS, dropped/replaced frame count, last-frame age, capture resolution, and pixel format.

## 5. Profiles and persistence

### SPEC-PROFILE-001 — Portable bundle

A portable profile shall be a self-contained directory or import/export archive containing:

- versioned profile document;
- thumbnail when present;
- relative template assets;
- optional declared model assets;
- manifest for exported archives.

### SPEC-PROFILE-002 — XDG locations

Default storage shall follow XDG locations:

```text
$XDG_CONFIG_HOME/yash-app-events/settings.toml
$XDG_CONFIG_HOME/yash-app-events/capture-bindings.toml
$XDG_DATA_HOME/yash-app-events/profiles/<profile-id>/
$XDG_STATE_HOME/yash-app-events/
$XDG_CACHE_HOME/yash-app-events/
$XDG_RUNTIME_DIR/yash-app-events/
```

Standard fallbacks apply when an XDG variable is unset.

### SPEC-PROFILE-003 — Stable identity

Profiles, elements, detectors, and rules shall have stable opaque IDs separate from display names. External event names shall be validated stable identifiers.

### SPEC-PROFILE-004 — Coordinates

Regions shall use normalized coordinates in `[0,1]` and record reference resolution, aspect ratio, UI scale, and language metadata where known.

Validation shall reject non-finite, negative, zero-area, or out-of-bounds regions.

### SPEC-PROFILE-005 — Atomic save

Profile and state snapshot writes shall use write-to-temporary-file, flush, and atomic rename within the same filesystem.

### SPEC-PROFILE-006 — Drafts and revisions

The GUI shall autosave recoverable drafts separately from the last committed profile. Committing a valid profile shall increment its revision. The system shall retain a bounded revision history, defaulting to 20 revisions.

### SPEC-PROFILE-007 — Optimistic concurrency

Mutating RPC requests shall provide the expected profile revision. A stale edit shall receive a conflict containing the current revision rather than overwriting newer work.

### SPEC-PROFILE-008 — Duplication

The system shall support duplication of profiles and elements.

Profile duplication shall:

- allocate a new profile ID;
- deep-copy portable assets;
- allocate new internal object IDs;
- preserve external event names unless a collision exists in the destination;
- reset revision history and timestamps;
- exclude capture bindings and restore tokens.

Element duplication shall allocate a new internal ID and copy detector settings. Event rules shall only be copied when explicitly requested.

### SPEC-PROFILE-009 — Schema migration

Every persisted profile shall include a schema version. Loading shall migrate older supported schemas into the current in-memory schema without overwriting the source until a successful explicit save.

Migration tests shall include golden fixtures for every historical schema version after version 1 ships.

### SPEC-PROFILE-010 — Import safety

Profile import shall defend against archive path traversal, symlinks, excessive file sizes, excessive total expansion, unsupported schemas, missing assets, and duplicate IDs. Imported content shall never be executed.

### SPEC-PROFILE-011 — Deletion

Profile deletion shall move data into an application-managed trash location. Permanent deletion shall be an explicit separate action.

## 6. Visual configuration UI

### SPEC-UI-001 — Toolkit

The initial GUI shall use Rust with `eframe`/`egui`, unless an implementation spike documents a blocking technical limitation.

### SPEC-UI-002 — Profile manager

The GUI shall list profiles and provide create, rename, duplicate, import, export, trash, restore, and activate operations.

### SPEC-UI-003 — Source setup

The GUI shall expose capture source selection, permission state, live status, capture metrics, preview start/stop, and frozen-frame inspection.

### SPEC-UI-004 — Region editor

The editor shall support:

- drawing, selecting, moving, resizing, duplicating, enabling, and disabling regions;
- zooming and panning without changing stored coordinates;
- visible label and color per region;
- normalized and reference-pixel coordinates;
- original crop and detector-preprocessed preview;
- current observation value, confidence, and diagnostic reason.

### SPEC-UI-005 — Detector editor

The UI shall expose detector-specific configuration, validation, and a test action against a frozen frame or replay frame.

### SPEC-UI-006 — Event-rule editor

The UI shall distinguish observations from events and visualize temporal evidence, current rule state, hysteresis, debounce windows, and cooldowns.

### SPEC-UI-007 — Timeline

The UI shall show recent observations, emitted transitions, confidence, timestamps, and diagnostics. Debug images shall only be persisted after explicit opt-in.

### SPEC-UI-008 — Responsiveness

Capture, image processing, OCR, inference, profile I/O, and output I/O shall not run on the GUI render thread.

## 7. Detection

### SPEC-DET-001 — Detector contract

A detector shall consume a region image plus context and produce an observation containing:

- detector and element IDs;
- timestamp;
- typed value;
- normalized confidence when meaningful;
- status (`valid`, `unknown`, or `error`);
- concise diagnostic metadata.

Detector failure shall not fabricate a negative observation.

### SPEC-DET-002 — Color bar detector

The first release shall include a configurable color/range bar detector supporting direction, color-space thresholds, masks, and a normalized fill result.

### SPEC-DET-003 — Template detector

The first release shall include normalized template matching with configurable threshold, multiple templates, optional masks, and best-match diagnostics.

### SPEC-DET-004 — Region-change detector

The first release shall include a region-change/stability detector suitable for triggering more expensive recognition or detecting loading transitions.

### SPEC-DET-005 — OCR

OCR shall be a detector backend introduced after deterministic detectors work end-to-end. The initial OCR implementation may use Tesseract or ONNX-based recognition, but model/runtime selection must be benchmarked using representative game HUD samples.

OCR is not required for the first vertical slice.

### SPEC-DET-006 — Classifier

Generic ONNX classification is `DEFERRED` until replay datasets and deterministic baseline metrics exist.

### SPEC-DET-007 — Preprocessing

Detector preprocessing shall be explicit, serializable, previewable, and deterministic. Supported operations should begin with crop, resize, grayscale, color conversion, threshold, and simple morphology.

## 8. Event engine

### SPEC-EVENT-001 — Separation

Detectors produce observations. Rules consume observation histories and produce state transitions. No detector may write directly to an output sink.

### SPEC-EVENT-002 — Rule primitives

The engine shall eventually support:

- numeric threshold crossing;
- boolean appearance/disappearance;
- string equality/contains;
- confidence threshold;
- N-of-M samples;
- stable duration;
- hysteresis with distinct enter/leave thresholds;
- cooldown;
- conjunction/disjunction of observations.

The first vertical slice requires numeric threshold, confidence threshold, N-of-M, hysteresis, and cooldown.

### SPEC-EVENT-003 — Transitions

Events shall be emitted on meaningful transitions such as `entered`, `updated`, and `left`, not once per analyzed frame. Update emission must be explicitly enabled and rate-limited.

### SPEC-EVENT-004 — Restart behavior

On daemon restart, detector history may reset. Output must identify a new daemon instance so consumers can distinguish restart from a continuous session. Initial state establishment shall not be reported as a transition unless configured.

## 9. Output contracts

### SPEC-OUT-001 — Event JSONL

The daemon shall append one compact JSON object per meaningful event transition to `events.jsonl`.

Required fields:

```json
{
  "schema": 1,
  "daemon_instance": "opaque-id",
  "sequence": 106,
  "timestamp": "2026-07-11T16:43:27.102Z",
  "profile_id": "opaque-id",
  "game": "blazblue_entropy_effect",
  "event": "critical_health",
  "state": "entered",
  "value": 0.17,
  "confidence": 0.91
}
```

### SPEC-OUT-002 — Current state

The daemon shall atomically replace `state.json` with the latest capture status, active profile, observations, and event states. It shall include schema, daemon instance, sequence, and update timestamp.

### SPEC-OUT-003 — Output durability

Flush policy and log rotation shall be configurable. Defaults must balance durability with avoiding an `fsync` for every frame; only transitions are appended.

### SPEC-OUT-004 — Failure behavior

Output failure shall be visible in daemon status, logs, IPC notifications, and the GUI. It shall not terminate capture unless explicitly configured as fatal.

## 10. IPC and CLI

### SPEC-IPC-001 — Local transport

The daemon shall expose JSON-RPC 2.0 over a Unix domain socket at:

```text
$XDG_RUNTIME_DIR/yash-app-events/control.sock
```

The socket shall be accessible only to the current user. TCP/WebSocket listeners are disabled by default and not required for the first release.

### SPEC-IPC-002 — Framing and limits

The Unix transport shall use a documented framing format, initially compact JSON objects delimited by newline. It shall enforce request size, nesting, connection, subscription-buffer, and timeout limits.

Binary frame data shall not be sent inline through JSON-RPC.

### SPEC-IPC-003 — Handshake

Clients shall negotiate a protocol version and identify their name/version before other calls. Incompatible versions shall receive a structured error.

### SPEC-IPC-004 — Methods

The version 1 API shall cover:

- system version, capabilities, status, and graceful shutdown;
- capture selection, start, stop, status, and snapshot;
- profile CRUD, activation, duplication, validation, import, and export;
- element CRUD, duplication, and detector testing;
- current state retrieval;
- event and status subscriptions;
- preview start and stop.

Method names and exact schemas shall be frozen in a separate protocol schema before the first public release.

### SPEC-IPC-005 — CLI

The CLI shall be a client of the IPC API for live operations. It shall provide human-readable output by default and stable machine-readable JSON with `--json`.

Offline operations may include profile validation and inspection without a running daemon, provided they use the same profile library.

### SPEC-IPC-006 — Subscription backpressure

Each subscription shall have a bounded buffer. A slow client shall not block the daemon; it shall receive an explicit lag notification or be disconnected according to documented policy.

## 11. Replay and testing

### SPEC-REPLAY-001 — Common engine path

Recorded images/video and live PipeWire capture shall feed the same detector and event engine interfaces.

### SPEC-REPLAY-002 — Determinism

Given the same profile, ordered frames, and timestamps, deterministic detectors and rules shall produce the same ordered observations and events.

### SPEC-REPLAY-003 — Fixtures

The repository shall support small redistributable synthetic fixtures. Copyrighted game footage shall not be committed without clear permission.

### SPEC-REPLAY-004 — Metrics

Replay evaluation should report event precision, recall, duplicates, misses, and detection latency. OCR character accuracy alone is not a product success metric.

## 12. Observability and diagnostics

### SPEC-OBS-001 — Structured logs

The daemon shall use structured logging with configurable verbosity. Logs shall avoid raw frame content and portal tokens.

### SPEC-OBS-002 — Runtime metrics

Status shall include capture and analysis rates, processing latency by detector, queue replacement counts, detector errors, output errors, and connected clients.

### SPEC-OBS-003 — Diagnostic bundle

A future diagnostic export may include logs, redacted configuration, metrics, and explicitly selected example crops. It must exclude secrets and full screenshots by default.

## 13. Performance targets

### SPEC-PERF-001 — Responsiveness

On reference hardware to be documented, the daemon should analyze configured small regions at 10 FPS without degrading capture stability. Exact CPU/GPU targets shall be established after the first benchmark harness exists.

### SPEC-PERF-002 — Idle cost

With capture stopped and no preview client, the daemon shall perform no periodic image work and should remain effectively idle.

### SPEC-PERF-003 — Profiling before optimization

Shared memory, DMA-BUF, GPU preprocessing, and custom OBS integration shall be considered only after profiling identifies frame transfer or conversion as a material bottleneck.

## 14. Security and privacy

### SPEC-SEC-001 — Local-only default

Control is local-only by default. No unauthenticated network listener shall be started.

### SPEC-SEC-002 — Profile trust boundary

Imported profiles, images, and models are untrusted. Parsing and inference shall have explicit resource limits and actionable errors.

### SPEC-SEC-003 — Capture visibility

The UI and CLI status shall clearly indicate whether capture is active and which source is selected.

### SPEC-SEC-004 — Debug image consent

Full frames and crops shall not be persisted unless the user explicitly requests a snapshot, enables diagnostic capture, or records a replay.

## 15. Compatibility policy

Before version `1.0`, internal Rust APIs may change freely. Persisted schemas, JSONL schemas, command names, and JSON-RPC methods become compatibility-sensitive as soon as a released version writes or exposes them.

Every external schema shall carry an explicit version. Breaking changes require a migration or a deliberate major protocol/schema version.

## 16. Requirements evidence index

As implementation proceeds, add entries in this format:

```text
SPEC-ID | STATUS | Evidence
SPEC-CAP-002 | VERIFIED | tests/portal_smoke.md and CI job linux-wayland-smoke
```

No requirements are verified at repository initialization.

SPEC-ARCH-002 | VERIFIED | Cargo workspace manifests and `docs/architecture.md`; `cargo fmt --all -- --check`, strict workspace Clippy, tests, and docs pass (2026-07-11)
SPEC-OBS-001 | IN_PROGRESS | `yash-app-eventsd` initializes configurable `tracing` output; structured operational fields and redaction tests remain pending
SPEC-PROFILE-002 | VERIFIED | XDG resolution tests plus atomic `settings.toml` and separate `capture-bindings.toml` round trips; portal tokens never enter portable profile trees (2026-07-11)
SPEC-PROFILE-003 | VERIFIED | typed UUID identities, stable-name validation, duplicate-ID and dangling-reference rejection in profile tests (2026-07-11)
SPEC-PROFILE-004 | VERIFIED | schema-v1 `NormalizedRegion` and layout metadata validation tests reject out-of-bounds regions with field paths (2026-07-11)
SPEC-PROFILE-005 | VERIFIED | same-directory temporary write, flush, sync, and rename; injected pre-rename failure test proves the prior document remains valid (2026-07-11)
SPEC-PROFILE-006 | VERIFIED | `ProfileStore` draft separation, revision increment/history pruning, and tests with configurable bounded history (2026-07-11)
SPEC-PROFILE-007 | VERIFIED | stale-commit test proves structured expected/current revision conflict without overwrite (2026-07-11)
SPEC-PROFILE-008 | VERIFIED | tests prove profile assets are deep-copied with all internal IDs rekeyed and element rules copy only on explicit request (2026-07-11)
SPEC-PROFILE-011 | VERIFIED | reversible application-managed trash/restore test; no implicit permanent deletion API (2026-07-11)
SPEC-PROFILE-001 | VERIFIED | `.hudprofile` ZIP export/import round trip includes schema-v1 manifest, profile document, portable assets, sizes, and SHA-256 integrity metadata (2026-07-11)
SPEC-PROFILE-009 | VERIFIED | explicit schema dispatcher rejects unsupported versions without source writes; reviewed `profile-v1.json` golden fixture loads in tests (2026-07-11)
SPEC-PROFILE-010 | VERIFIED | staged import validates enclosed paths, ZIP link modes, declared entries, hashes, schemas, IDs/assets, per-file/count/total limits; malicious fixtures prove traversal, symlink, and expansion rejection (2026-07-11)
SPEC-ARCH-001 | IN_PROGRESS | daemon owns `ProfileStore`; concurrent GUI/CLI protocol clients are supported, capture/engine ownership remains pending
SPEC-IPC-001 | VERIFIED | Tokio Unix-socket integration tests verify documented path configuration, runtime dir 0700, socket 0600, safe stale recovery, and no network listener (2026-07-11)
SPEC-IPC-002 | VERIFIED | newline-framed compact JSON with 1 MiB message, depth-32 nesting, connection, and bounded subscription limits; protocol golden and transport tests (2026-07-11)
SPEC-IPC-003 | VERIFIED | transport test rejects pre-handshake methods and accepts protocol-v1 identification; incompatible version has stable structured code (2026-07-11)
SPEC-IPC-004 | IN_PROGRESS | version/capabilities/status/shutdown, core profile operations, state, and subscriptions implemented; capture, full profile lifecycle, detector, and preview methods pending
SPEC-IPC-006 | VERIFIED | capacity-64 per-subscriber broadcast path emits `subscription.lagged`; bounded-channel test proves overwrite/lag behavior (2026-07-11)
SPEC-IPC-005 | VERIFIED | `yash-eventsctl` is a negotiated RPC client with global compact `--json`, stable exit categories, timeouts, live event follow, profile lifecycle commands, and shared-library offline validation; golden and daemon-backed tests (2026-07-11)
SPEC-CAP-001 | VERIFIED | backend-neutral validated CPU frame carries monotonic timestamp, dimensions, padded stride, RGB/RGBA format, memory bytes, and source identity; portal callback tests (2026-07-11)
SPEC-ARCH-003 | VERIFIED | `LatestFrameSlot` owns at most one frame; 10,000-frame test proves 9,999 replacements and next analysis receives sequence 9,999 (2026-07-11)
SPEC-PROD-004 | VERIFIED | local settings validate configurable 1–10 FPS; live latest-frame worker test feeds 60 timestamped FPS, analyzes at most 10, replaces 59 stale frames, and emits one transition (2026-07-11)
SPEC-DET-001 | VERIFIED | `FrameProcessor` attaches stable detector/element IDs to typed value/status/confidence/diagnostic results; errors retain no fabricated value; replay integration evidence (2026-07-11)
SPEC-EVENT-001 | VERIFIED | detector output becomes an observation, `NumericRule` alone creates transitions, and only transitions reach sinks in daemon replay integration (2026-07-11)
SPEC-EVENT-002 | IN_PROGRESS | numeric threshold, confidence, N-of-M, hysteresis, and cooldown implemented and tested; remaining eventual primitives pending
SPEC-EVENT-003 | VERIFIED | synthetic health history emits exactly `entered` then `left`; no per-frame output and low-confidence/unknown samples add no false evidence (2026-07-11)
SPEC-OUT-001 | VERIFIED | `EventRecord` golden test proves one compact schema-v1 JSON object per transition with all required fields (2026-07-11)
SPEC-OUT-002 | VERIFIED | schema-v1 snapshot includes daemon instance/sequence/timestamp/capture/profile/observations/events; atomic interruption and daemon `state.get` equality tests (2026-07-11)
SPEC-OUT-003 | VERIFIED | configurable transition flush count and size-based single-generation rotation implemented; JSONL golden test flushes and reads output (2026-07-11)
SPEC-OUT-004 | IN_PROGRESS | output errors are typed/non-panicking and failure-injection tested; daemon status/log/IPC/GUI surfacing pending
SPEC-DET-002 | VERIFIED | four-direction RGB range/mask detector handles padded stride, partial/scaled bars, noise and brightness variation; profile-backed daemon RPC plus replay produces files/state/subscription events (2026-07-11)
SPEC-REPLAY-001 | IN_PROGRESS | `ReplaySource` frames feed the same `FrameProcessor<Detector>` boundary intended for live capture; daemon live wiring pending
SPEC-REPLAY-002 | VERIFIED | identical timestamped synthetic health frames run twice through color detection and temporal rules and yield identical ordered entered/left transitions (2026-07-11)
SPEC-REPLAY-003 | IN_PROGRESS | redistributable in-memory synthetic health fixtures cover bounded frames and transitions; repository manifest/fixture format pending
SPEC-ARCH-004 | VERIFIED | monotonic `Duration` orders frames/rules; UTC millisecond RFC 3339 external timestamps, per-instance UUID, and increasing event sequence are asserted across files/state/IPC (2026-07-11)
SPEC-EVENT-004 | VERIFIED | first N-of-M state establishment produces no transition; each daemon creates a UUID instance carried by state and events (2026-07-11)
SPEC-DET-003 | VERIFIED | multi-template normalized matching with masks/assets/best diagnostics and brightness test; profile replay integration asserts entered/left records identical in JSONL and RPC (2026-07-11)
SPEC-DET-004 | VERIFIED | normalized change/stability unknown-baseline behavior plus profile replay integration asserts left/entered records identical in JSONL/RPC and final state (2026-07-11)
SPEC-DET-007 | VERIFIED | schema-v1 serializable grayscale/resize/threshold/erode/dilate/invert pipeline reproduces preview pixels; `detector.test` returns bounded compressed PNG preview with no persistence (2026-07-11)
SPEC-PERF-003 | VERIFIED | release-mode three-detector baseline and reference CPU recorded in `docs/performance.md`; results do not justify advanced transfer/GPU optimization (2026-07-11)
SPEC-PROD-002 | IN_PROGRESS | ashpd portal/direct PipeWire backend builds on live Hyprland/Wayland dependencies; interactive selection/cancel/deny and second-backend evidence pending
SPEC-CAP-002 | IN_PROGRESS | create/select/start/open-remote/session-close flow implemented with hidden cursor and actionable error categories; interactive smoke evidence pending
SPEC-CAP-003 | IN_PROGRESS | machine-local token persistence, portal ExplicitlyRevoked mode, reuse, and stale-token explicit fallback implemented; interactive restoration evidence pending
SPEC-CAP-004 | VERIFIED | negotiation requests RGB/RGBA/RGBx only; tests verify padded copies, RGBx alpha normalization, and actionable short/unsupported diagnostics (2026-07-11)
SPEC-CAP-006 | IN_PROGRESS | callback and daemon/CLI expose input/analysis FPS, replacements, frame age, resolution, format/error; interactive portal metric evidence pending
SPEC-SEC-003 | VERIFIED | shared system/capture status and CLI expose active flag and selected portal node label (2026-07-11)
SPEC-SEC-004 | IN_PROGRESS | backend persists nothing; snapshot RPC requires explicit destination and padded-frame PNG/atomic writer tests pass; manual portal filesystem smoke pending
SPEC-PERF-001 | IN_PROGRESS | live-worker 60-FPS synthetic test remains bounded and emits output at configured analysis rate; real portal/reference CPU end-to-end profile pending
SPEC-PERF-002 | IN_PROGRESS | daemon has no image timer until a profile-backed capture starts and analysis task stops with capture; idle CPU measurement pending
SPEC-UI-001 | VERIFIED | `yash-app-events` uses eframe/egui 0.32 and completed a five-second native Wayland startup smoke with daemon connection (2026-07-11)
SPEC-UI-002 | IN_PROGRESS | GUI exposes create/rename/duplicate/import/export/trash/restore/activate over shared RPC; interaction acceptance smoke pending
SPEC-UI-003 | IN_PROGRESS | GUI exposes portal select/stop, permission errors, metrics, preview/freeze; interactive portal acceptance pending
SPEC-UI-004 | IN_PROGRESS | normalized canvas supports draw/select/move/resize/duplicate/enable, aspect-preserving zoom/pan, labels and reference pixels; crop/processed/observation panels pending Phase 7
SPEC-UI-008 | VERIFIED | GUI render thread only mutates widget/texture state; dedicated worker owns RPC, reconnect/timeouts and PNG decode; daemon owns all capture/detection/I/O (2026-07-11)
SPEC-CAP-005 | IN_PROGRESS | per-connection opt-in lease, 320x180 PNG downscale, disconnect cleanup and no-detector-input path pass tests; interactive preview smoke pending
