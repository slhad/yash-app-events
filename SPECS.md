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

Revision history shall be available through the shared control protocol. Clients shall
be able to list and inspect retained snapshots, compare them with the current profile,
and roll a selected snapshot forward as a new revision after optimistic-concurrency
validation. Rollback shall never erase or overwrite the current revision.

Profiles may define stable-ID derived text observations with an enabled state, a bounded
format string, and named inputs referencing detector observations. The daemon shall own
composition and publish derived values through the same state, rule, output, and JSON-RPC
paths as detector observations. The GUI shall expose the hierarchy and editable composition.

Event rules may detect a bounded rapid numeric increase over a configured time window.
Digit-only text observations shall be accepted as numeric inputs. Emission shall retain
stable rule/event IDs and obey cooldown, enabling downstream recording consumers without
embedding recording actions in detection.

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

### SPEC-PROFILE-012 — Public profile catalog

The application shall browse one rolling GitHub release with stable tag `profiles` and
display title `Profile Catalog`. Profile archives shall use immutable names of the form
`profile--<game-slug>--<profile-slug>--v<semantic-version>.hudprofile`; catalog documents
shall be append-only `catalog-v1-rNNNNNN.json` revisions. Publication shall upload new
packages before the index that references them and shall never overwrite an existing asset.

Catalog access shall be daemon-owned and exposed to GUI and CLI through the same protocol.
The daemon shall use a fixed repository/release origin, bounded HTTPS timeouts and response
sizes, strict schema/compatibility validation, atomic XDG cache writes, and package
size/SHA-256 verification before handing a download to the existing safe profile importer.
The last valid cached catalog shall remain usable while offline. Installation shall require
the reviewed catalog revision and package hash, shall reject withdrawn or duplicate versions,
and shall never activate a profile or authorize an output route automatically.

Catalog source versions shall be reviewable, append-only repository directories. A
`media_free` source shall reject images, video, audio, binary/undeclared files, symlinks,
absolute paths, capture bindings, restore tokens, machine-local routes, secrets, credentials,
and likely email addresses. The generated package shall contain only portable profile content
and validated inert output recipes. Private regression media may verify a package but shall
not be published with it.

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

The daemon shall optionally monitor a machine-local game-process match for the active
profile. After bounded debounce it shall restore capture when the process appears and
stop capture when it disappears. Process policy and portal tokens shall remain outside
portable profiles and shall be controllable through the shared JSON-RPC protocol.

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

Optional OCR HUD counters may configure an explicit empty value. When their visual token
disappears, the detector shall emit that value as valid rather than retaining stale text;
ordinary OCR detectors shall keep transient-empty retention behavior.

### SPEC-DET-006 — Classifier

Generic ONNX classification shall load explicitly declared portable model assets only
after validating their path, size, SHA-256, input dimensions, ordered labels, and
resource scheduling. It shall emit typed text-label observations and use the common
temporal-rule path. Model failures shall produce `unknown` or `error`, never a
fabricated negative classification.

Acceptance:

- A redistributable dataset and model manifest record expected labels and model hash.
- CPU, memory, latency, confidence, and exact-accuracy baselines are reproducible.
- Unchanged crops do not cause unbounded inference; a forced refresh remains configurable.
- Profile, daemon, GUI, frozen test, image replay, and event integration pass.

### SPEC-DET-007 — Preprocessing

Detector preprocessing shall be explicit, serializable, previewable, and deterministic. Supported operations should begin with crop, resize, grayscale, color conversion, threshold, and simple morphology.

### SPEC-DET-008 — Seven-segment recognition

Fixed-layout seven-segment HUD text shall use deterministic glyph decoding rather than
general-purpose OCR. Profiles shall declare the digit count, optional separator
position, brightness threshold, and preprocessing. Invalid or ambiguous glyphs shall
produce `unknown`, never a fabricated digit.

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

### SPEC-EVENT-005 — Complete typed rule language

The post-release rule language shall support boolean appearance/disappearance, string
equality and substring matching, stable-duration evidence, and conjunction/disjunction
over bounded sets of element observations. Composition shall be non-recursive and
shall reference stable element IDs so cyclic rule evaluation is impossible.

Initial state emission shall be explicitly configurable. Active-state `updated`
transitions shall be opt-in and rate-limited. Unknown, erroneous, missing, or
type-mismatched observations shall not fabricate negative evidence.

Acceptance:

- Schema-v1 numeric profiles retain their existing semantics without migration.
- Every predicate round-trips through profiles and can be authored in the GUI.
- Unit and replay tests cover boolean, string, stable-duration, all/any composition,
  initial establishment, entered, updated, and left behavior.
- Live and replay paths use the same rule runtime and publish identical transitions
  through JSONL, state, and IPC.
- Composition contains at most 16 observation leaves and profile duplication rekeys
  every referenced element ID.

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

### SPEC-OUT-005 — Profile-scoped output routes

The daemon shall support bounded machine-local output routes keyed by profile ID. A route
shall select meaningful event transitions or rendered state changes, render either the
stable full contract, a bounded JSON template, or a bounded raw-text template, and deliver it to an append/replace file
or a directly executed local program. Route execution shall not use a shell.

Command routes are executable local policy, not portable profile content. They shall be
disabled until explicitly configured on the machine, require an absolute executable path,
receive compact JSON on standard input, have bounded arguments and timeout, and run outside
capture/analysis and GUI render threads. Imported profiles shall never create or authorize
command routes.

Acceptance:

- At most 64 routes per profile and 64 pending deliveries are retained.
- Event routes filter stable event names and `entered`/`updated`/`left` states.
- State routes compare the rendered payload and do not deliver an unchanged template.
- JSON placeholders preserve typed values when the placeholder is the complete JSON string.
- Raw-text templates write UTF-8 without JSON quoting, with an explicit default-on
  trailing line feed and no carriage return; disabling the line feed writes no line-ending
  bytes. Atomic replace supports a universally readable current-value file.
- The shared protocol and CLI can list, set, enable/disable, remove, and test routes.
- The GUI lists routes and exposes enable/disable plus an explicit delivery test.
- Profile replay publishes through the same durable state/event and output-route boundary as
  live processing; long image/OCR replays remain bounded and visible in the GUI.
- Delivery failures remain non-fatal and appear in status and live notifications.

### SPEC-OUT-006 — Portable inert output recipes

Portable profiles may carry bounded schema-1 output recipes under `output-recipes/`.
A recipe may describe an event/state trigger, JSON or raw-text format, and suggested file or command
sink, but shall contain neither an authorized filesystem destination nor an executable
path or enabled state. Import/export shall validate, hash, and resource-limit recipes.

The GUI shall list packaged recipes, disclose their source path/hash, trigger, payload,
suggested sink, and description, and allow trigger/payload editing. Preview shall render
sample JSON without side effects. Installation shall require an explicit absolute local
file or executable selection, revalidate the reviewed recipe hash, record immutable source
provenance, allocate a local route ID, and always install disabled. Delivery testing and
enabling remain separate explicit actions.

Acceptance:

- Imported recipes cannot authorize or execute a route.
- At most 32 recipe files of at most 64 KiB each are accepted per profile.
- Unsafe entries, malformed recipes, duplicate recipe IDs, and changed review hashes fail.
- Portable archive round trips retain recipe manifest hashes and semantics.
- GUI list/edit/preview/install uses shared versioned daemon methods.
- Editing or manually replacing an installed route removes recipe provenance rather than
  falsely presenting it as the reviewed recipe.

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

### SPEC-REPLAY-005 — External detector suites

The daemon shall evaluate schema-versioned detector-regression packages located outside
the repository without installing or mutating their pinned portable profile. Packages
shall support full frames, partial screenshots with explicit reference-frame placement,
and exact detector-zone crops. Each case shall declare typed expected observations,
optional event expectations, purpose, categories, and source-media provenance. A
SHA-256 inventory and canonical path checks shall reject changed or escaping files.
The CLI shall expose the same JSON-RPC method, stable JSON results, and a nonzero exit
status when any assertion or checked event regression fails.

### SPEC-REPLAY-006 — Passive evidence collection and review

The daemon shall optionally collect exact analyzed game frames into an external
regression package while capture is active. The machine-local per-profile policy shall
default to a 70-second interval, support bounded jitter, perceptual-similarity and
detector-evidence deduplication, storage/item quotas, and explicitly selected novelty
targets. Each item shall atomically retain the frame, pinned profile revision/hash,
observations, transitions, source metadata, image hash, and an unverified review state.

Pending evidence shall never become ground truth without review. The shared versioned
control protocol, CLI, and GUI shall support listing, inspection, accept, correction,
rejection, conservative automatic batch review, comparison, and promotion. Automation
shall leave ambiguous observations as `needs_correction`; promotion shall create a
checksummed external-suite case without mutating or installing the portable profile.

## 12. Observability and diagnostics

### SPEC-OBS-001 — Structured logs

The daemon shall use structured logging with configurable verbosity. Logs shall avoid raw frame content and portal tokens.

### SPEC-OBS-002 — Runtime metrics

Status shall include capture and analysis rates, processing latency by detector, queue replacement counts, detector errors, output errors, and connected clients.

### SPEC-OBS-003 — Diagnostic bundle

A diagnostic export may include bounded logs, redacted configuration, metrics, and
explicitly selected example crops. It must exclude secrets, machine-local capture
bindings, portal tokens, and full screenshots by default. The user shall see the exact
entry list, sizes, and a privacy warning before authorizing export.

Acceptance:

- Recursive adversarial fixtures prove secrets and machine-local bindings are absent.
- Only explicitly selected frozen-frame element regions become image entries.
- Per-file, crop-count, log-count, and total uncompressed limits are enforced.
- Export is atomic and a failed export leaves the previous destination intact.
- GUI and CLI use the same versioned plan/review/export protocol.

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
SPEC-OBS-001 | VERIFIED | daemon initializes `tracing` with configurable `RUST_LOG`, structured startup fields, and no frame/token logging paths; security review records redaction boundary (2026-07-11)
SPEC-PROFILE-002 | VERIFIED | XDG resolution tests plus atomic `settings.toml` and separate `capture-bindings.toml` round trips; portal tokens never enter portable profile trees (2026-07-11)
SPEC-PROFILE-003 | VERIFIED | typed UUID identities, stable-name validation, duplicate-ID and dangling-reference rejection in profile tests (2026-07-11)
SPEC-PROFILE-004 | VERIFIED | schema-v1 `NormalizedRegion` and layout metadata validation tests reject out-of-bounds regions with field paths (2026-07-11)
SPEC-PROFILE-005 | VERIFIED | same-directory temporary write, flush, sync, and rename; injected pre-rename failure test proves the prior document remains valid (2026-07-11)
SPEC-PROFILE-006 | VERIFIED | `ProfileStore` draft separation, revision increment/history pruning, protocol list/get/rollback with expected-revision conflicts, roll-forward preservation tests, and GUI history/comparison/confirmation workflow (2026-07-12)
SPEC-PROFILE-007 | VERIFIED | stale-commit test proves structured expected/current revision conflict without overwrite (2026-07-11)
SPEC-PROFILE-008 | VERIFIED | tests prove profile assets are deep-copied with all internal IDs rekeyed and element rules copy only on explicit request (2026-07-11)
SPEC-PROFILE-011 | VERIFIED | reversible application-managed trash/restore test; no implicit permanent deletion API (2026-07-11)
SPEC-PROFILE-001 | VERIFIED | `.hudprofile` ZIP export/import round trip includes schema-v1 manifest, profile document, portable assets, sizes, and SHA-256 integrity metadata (2026-07-11)
SPEC-PROFILE-009 | VERIFIED | explicit schema dispatcher rejects unsupported versions without source writes; reviewed `profile-v1.json` golden fixture loads in tests (2026-07-11)
SPEC-PROFILE-010 | VERIFIED | staged import validates enclosed paths, ZIP link modes, declared entries, hashes, schemas, IDs/assets, per-file/count/total limits; malicious fixtures prove traversal, symlink, and expansion rejection (2026-07-11)
SPEC-PROFILE-012 | VERIFIED | 124-test workspace suite and strict Clippy pass; `profiles` publication run 29623649827 produced immutable package SHA-256 `07efe534ec49d723cd4ce06fa6ea0becc085ee4b71ba63288e97ec9f612c05b4` plus `catalog-v1-r000001.json`; downloaded bytes match a local deterministic rebuild; a fresh daemon fetched, cached, verified, and installed the profile inactive with two inert recipes and zero routes; native workspace-4 GUI review passed without cua-driver (2026-07-18)
SPEC-ARCH-001 | VERIFIED | daemon exclusively owns profiles, portal session, latest-frame/analysis worker, outputs, and protocol state; GUI/CLI are protocol-v1 clients and render thread performs no I/O (2026-07-11)
SPEC-IPC-001 | VERIFIED | Tokio Unix-socket integration tests verify documented path configuration, runtime dir 0700, socket 0600, safe stale recovery, and no network listener (2026-07-11)
SPEC-IPC-002 | VERIFIED | newline-framed compact JSON with 1 MiB message, depth-32 nesting, connection, and bounded subscription limits; protocol golden and transport tests (2026-07-11)
SPEC-IPC-003 | VERIFIED | transport test rejects pre-handshake methods and accepts protocol-v1 identification; incompatible version has stable structured code (2026-07-11)
SPEC-IPC-004 | VERIFIED | documented protocol-v1 implements system, complete profile lifecycle, capture/snapshot, detector/template test, replay, state, bounded subscriptions, and preview lease/freeze methods with daemon integration evidence (2026-07-11)
SPEC-IPC-006 | VERIFIED | capacity-64 per-subscriber broadcast path emits `subscription.lagged`; bounded-channel test proves overwrite/lag behavior (2026-07-11)
SPEC-IPC-005 | VERIFIED | `yash-eventsctl` is a negotiated RPC client with global compact `--json`, stable exit categories, timeouts, live event follow, profile lifecycle commands, and shared-library offline validation; golden and daemon-backed tests (2026-07-11)
SPEC-CAP-001 | VERIFIED | backend-neutral validated CPU frame carries monotonic timestamp, dimensions, padded stride, RGB/RGBA format, memory bytes, and source identity; portal callback tests (2026-07-11)
SPEC-ARCH-003 | VERIFIED | `LatestFrameSlot` owns at most one frame; 10,000-frame test proves 9,999 replacements and next analysis receives sequence 9,999 (2026-07-11)
SPEC-PROD-004 | VERIFIED | local settings validate configurable 1–10 FPS; live latest-frame worker test feeds 60 timestamped FPS, analyzes at most 10, replaces 59 stale frames, and emits one transition (2026-07-11)
SPEC-DET-001 | VERIFIED | `FrameProcessor` attaches stable detector/element IDs to typed value/status/confidence/diagnostic results; errors retain no fabricated value; replay integration evidence (2026-07-11)
SPEC-EVENT-001 | VERIFIED | detector output becomes an observation, `NumericRule` alone creates transitions, and only transitions reach sinks in daemon replay integration (2026-07-11)
SPEC-EVENT-002 | VERIFIED | all first-usable-slice primitives—numeric threshold, confidence, N-of-M, hysteresis, and cooldown—are schema-backed, GUI-editable, and engine tested; boolean/string/composition remain post-release candidates under the normative “eventually” scope (2026-07-11)
SPEC-EVENT-003 | VERIFIED | synthetic health history emits exactly `entered` then `left`; no per-frame output and low-confidence/unknown samples add no false evidence (2026-07-11)
SPEC-OUT-001 | VERIFIED | `EventRecord` golden test proves one compact schema-v1 JSON object per transition with all required fields (2026-07-11)
SPEC-OUT-002 | VERIFIED | schema-v1 snapshot includes daemon instance/sequence/timestamp/capture/profile/observations/events; atomic interruption and daemon `state.get` equality tests (2026-07-11)
SPEC-OUT-003 | VERIFIED | configurable transition flush count and size-based single-generation rotation implemented; JSONL golden test flushes and reads output (2026-07-11)
SPEC-OUT-004 | VERIFIED | typed sink failures never panic, failure injection preserves engine operation, daemon records status `output_error` and emits a live error notification consumed by protocol clients/GUI (2026-07-11)
SPEC-OUT-005 | VERIFIED | machine-local schema-1 routes provide event filters, rendered-state deduplication, typed JSON and raw-text templates, configurable trailing line feed, append/atomic-replace files, direct bounded commands without a shell, capacity-64 background delivery, shared list/set/enable/remove/test RPC and CLI, GUI enable/test controls, and output/profile/daemon integration tests; configured profile replay publishes through the same output boundary, isolated workspace-4 GUI runs proved both the 13-byte `STAGE-3 : 04\n` file and exact 12-byte no-line-ending variant, and visual acceptance confirms the route section and individual routes are collapsed by default with enabled-count/actionable summaries while expanded Trigger/Sink metadata remains normal-sized (2026-07-18)
SPEC-OUT-006 | VERIFIED | schema-1 inert recipes are limited/validated/hashed during portable archive import/export; hash-pinned preview has no side effect, install requires an explicit absolute local sink and creates a fresh disabled route with provenance, manual replacement clears provenance, and shared RPC plus GUI browse/edit/preview/install are covered by output/profile/archive/daemon integration tests; packaged examples are collapsed by default; strict Clippy, 117 workspace tests, README claims, and docs pass (2026-07-18)
SPEC-DET-002 | VERIFIED | four-direction RGB range/mask detector handles padded stride, partial/scaled bars, noise and brightness variation; profile-backed daemon RPC plus replay produces files/state/subscription events (2026-07-11)
SPEC-REPLAY-001 | VERIFIED | live latest-frame and replay manifest paths both feed the configured detector, derived observations, temporal rules, durable state/events, and profile output routes; daemon integration tests plus an isolated workspace-4 GUI run exercise both, and the GUI permits up to 300 seconds for bounded OCR/image replay completion (2026-07-17)
SPEC-REPLAY-002 | VERIFIED | identical timestamped synthetic health frames run twice through color detection and temporal rules and yield identical ordered entered/left transitions (2026-07-11)
SPEC-REPLAY-003 | VERIFIED | schema-v1 bounded synthetic manifest format, detector-specific fixture semantics, validation, and redistributable deterministic integration cases are documented in `docs/replay.md` (2026-07-11)
SPEC-REPLAY-004 | VERIFIED | engine evaluator and daemon/CLI report precision, recall, duplicates, misses, mean event latency, pass/fail thresholds, stable JSON, and regression exit status 7; tests cover passing known events and duplicate regression (2026-07-11)
SPEC-REPLAY-005 | VERIFIED | schema-v1 external packages pin a portable profile and SHA-256 inventory; daemon/CLI `suite.evaluate` safely evaluate full frames, positioned partial screenshots, and detector-zone crops through the common detector/derived-observation/event path; typed exact/tolerant assertions, event metrics, category summaries, traversal rejection, and exit status 7 are tested; the ignored local BlazBlue suite passes 33 cases, 50 frames, and 126 observation assertions (2026-07-14)
SPEC-REPLAY-006 | VERIFIED | opt-in machine-local 70±10-second collection stores exact analyzed frames and full evidence atomically only during active game capture; a 32×18 perceptual comparison identifies the real 10-second static pair at difference 0.008501 below the 0.015 threshold; quotas, novelty signatures, hash verification, review states, conservative batch review, correction, rejection, and checksum-updating promotion are covered by engine/daemon/CLI tests and shared JSON-RPC/GUI controls; 12 corrected live items promoted into the external BlazBlue suite and all pass (2026-07-14)
SPEC-ARCH-004 | VERIFIED | monotonic `Duration` orders frames/rules; UTC millisecond RFC 3339 external timestamps, per-instance UUID, and increasing event sequence are asserted across files/state/IPC (2026-07-11)
SPEC-EVENT-004 | VERIFIED | first N-of-M state establishment produces no transition; each daemon creates a UUID instance carried by state and events (2026-07-11)
SPEC-EVENT-005 | VERIFIED | schema-v1-compatible typed predicates and bounded non-recursive composition validate/round-trip/rekey; boolean/text/stable/initial/updated/all/any engine tests, GUI authoring, multi-element live coordinator, and numeric/OCR/classifier image replay event evidence pass (2026-07-11)
SPEC-DET-003 | VERIFIED | multi-template normalized matching with masks/assets/best diagnostics and brightness test; profile replay integration asserts entered/left records identical in JSONL and RPC (2026-07-11)
SPEC-DET-004 | VERIFIED | normalized change/stability unknown-baseline behavior plus profile replay integration asserts left/entered records identical in JSONL/RPC and final state (2026-07-11)
SPEC-DET-007 | VERIFIED | schema-v1 serializable grayscale/resize/threshold/erode/dilate/invert pipeline reproduces preview pixels; `detector.test` returns bounded compressed PNG preview with no persistence (2026-07-11)
SPEC-PERF-003 | VERIFIED | release-mode three-detector baseline and reference CPU recorded in `docs/performance.md`; results do not justify advanced transfer/GPU optimization (2026-07-11)
SPEC-PROD-002 | VERIFIED | supported CachyOS/Hyprland portal selection delivered 3840×2160 frames through PipeWire 1.6.6 and stopped cleanly; isolated fresh-permission cancellation returns an actionable error and typed policy denial is tested (2026-07-11)
SPEC-CAP-002 | VERIFIED | live Hyprland create/select/start/open-remote/frame/session-close succeeds with hidden cursor; real isolated chooser cancellation, readiness-race propagation, and typed NotAllowed denial behavior pass (2026-07-11)
SPEC-CAP-003 | VERIFIED | machine-local token persistence, portal ExplicitlyRevoked mode, reuse, stale-token explicit fallback tests, and live Hyprland restoration pass; a stopped 3840×2160 profile capture restored without a picker in 35 ms and reported `restore_token_saved: true` (2026-07-11)
SPEC-CAP-004 | VERIFIED | live Hyprland exposed the need for BGR-family negotiation; RGB/RGBA/RGBx/BGR/BGRA/BGRx are supported with padded copies, channel/alpha normalization tests, and actionable short/unsupported diagnostics (2026-07-11)
SPEC-CAP-006 | VERIFIED | callback format/stride tests and daemon live-worker integration verify input/analysis rates, replacements, frame age, resolution, format/error and detector latency/error counters through status RPC/CLI/GUI (2026-07-11)
SPEC-SEC-003 | VERIFIED | shared system/capture status and CLI expose active flag and selected portal node label (2026-07-11)
SPEC-SEC-004 | VERIFIED | capture callback has no persistence path; snapshot/template RPCs require explicit actions/destinations and padded-frame PNG/atomic tests pass; security review enumerates all image persistence paths (2026-07-11)
SPEC-PERF-001 | VERIFIED | reference Ryzen 7 5800X3D release benchmarks keep deterministic small-region detectors below 0.52 ms/evaluation and live-worker 60-FPS input remains bounded at 10 analysis FPS with expected output (2026-07-11)
SPEC-PERF-002 | VERIFIED | release daemon with stopped capture/no preview measured 0 CPU scheduler ticks over two seconds at CLK_TCK=100; image task lifecycle and prompt stop are tested/documented in `docs/performance.md` (2026-07-11)
SPEC-UI-001 | VERIFIED | `yash-app-events` uses eframe/egui 0.32 and completed a five-second native Wayland startup smoke with daemon connection (2026-07-11)
SPEC-UI-002 | VERIFIED | GUI exposes list/create/rename-by-commit/duplicate/import/export/trash/restore/activate over the same revision-aware protocol methods tested by CLI/daemon integration; native Wayland startup smoke passes (2026-07-11)
SPEC-UI-003 | VERIFIED | native GUI source selection, permission/capture state, live preview/freeze/metrics, interactive request progress, and daemon-late reconnect/profile recovery pass on Hyprland with screenshot/RPC evidence in `docs/gui-acceptance-report.md` (2026-07-11)
SPEC-UI-004 | VERIFIED | normalized canvas supports draw/select/move/resize/duplicate/enable, explicit named zone listing/selection, aspect-preserving zoom/pan, labels/reference pixels, original and processed crop panels, and observation diagnostics; native screenshot evidence is in `docs/gui-acceptance-report.md` (2026-07-11)
SPEC-UI-008 | VERIFIED | GUI render thread only mutates widget/texture state; dedicated worker owns RPC, reconnect/timeouts and PNG decode; daemon owns all capture/detection/I/O (2026-07-11)
SPEC-CAP-005 | VERIFIED | per-connection opt-in lease, bounded caller-sized PNG previews capped at 1600x900, frozen exact-frame testing, disconnect cleanup and no-detector-input path pass daemon/live-worker tests; live protocol acceptance returned 1600x900 independently of detector input (2026-07-11)
SPEC-PROD-001 | VERIFIED | installed GUI selects a live source, lists/persists normalized zones and detector/rule configuration, exposes live observations, and the common engine E2E publishes resulting transitions to JSONL/state plus bounded IPC; native/replay evidence is recorded in `docs/gui-acceptance-report.md` (2026-07-11)
SPEC-PROD-003 | VERIFIED | capture is portal-mediated and the codebase has no process-memory, injection, input synthesis, or anti-cheat interface; architecture/security review (2026-07-11)
SPEC-UI-005 | VERIFIED | detector-specific color/template/change forms, preprocessing, validation through commit, draft-aware frozen/live tests, template capture, and original/processed diagnostics are implemented through protocol-v1; frozen region-change acceptance returned baseline plus a valid sample (2026-07-11)
SPEC-UI-006 | VERIFIED | numeric rule editor explicitly presents observation versus event, enter/leave hysteresis, confidence, N-of-M evidence, cooldown, and current state (2026-07-11)
SPEC-UI-007 | VERIFIED | always-visible live evidence panel and bounded timeline display capture state/metrics, observations, event states, detector value/confidence/diagnostic, and transition sequence/time; image persistence remains explicit (2026-07-11)
SPEC-DET-005 | VERIFIED | redistributable English/localized/scale/animation/glow fixtures and reproducible Tesseract 5 versus RapidOCR/ONNX Runtime accuracy/latency/confidence/CPU/memory benchmark select Tesseract; typed native detector, bounded change-triggered refresh with binary retry and last-valid retention on transient empty reads, profile validation, daemon/GUI configuration, frozen diagnostics, fixture regression tests, and common-path image replay text event pass; recorded BlazBlue structured-stage replay recognizes group `2` and counters `05` and `10`, composing `STAGE-2 : 10` (2026-07-12)
SPEC-DET-008 | VERIFIED | generic fixed-layout seven-segment detector validates digit/separator layout, discovers glyph bounds, handles narrow `1` glyphs, emits typed text/confidence/diagnostics, supports GUI/profile/image replay, and decoded live BlazBlue timer `31:44`, `32:04`, `32:07`, and `36:10` exactly at 3840×2160 (2026-07-12)
SPEC-DET-006 | VERIFIED | generated eight-case noisy/shifted orb-versus-cross HUD-icon dataset and 765-byte ONNX model with manifest/hash; path/size/SHA/labels/dimensions/output/scheduling validation, bounded change cache, CPU `ort` inference, GUI configuration/diagnostics, daemon image replay and text event pass; 80,000-case release benchmark records 100% fixture accuracy, confidence, 0.00333 ms latency, 26 CPU ticks and 26792 KiB peak RSS; clean-prefix installed-daemon replay returns typed labels and precision/recall 1.0 (2026-07-11)
SPEC-OBS-002 | VERIFIED | status/capture RPC and GUI/CLI expose input/analysis FPS, processing latency, replacements, detector/output errors, frame age/resolution/format, connected clients, and process-wide daemon/GUI CPU plus RSS memory (2026-07-12)
SPEC-OBS-003 | VERIFIED | protocol-v1 plan/review/export is shared by CLI and GUI; exact entry/size disclosure, visible privacy confirmation, recursive secret/binding/token redaction, explicit frozen element crops, PNG/name/count/file/total limits, atomic ZIP output, failure cleanup, and daemon/output adversarial tests pass (2026-07-11)
SPEC-SEC-001 | VERIFIED | Unix-only socket with private runtime directory/socket modes, safe stale recovery, connection/message limits, and no network listener; integration tests and security review (2026-07-11)
SPEC-SEC-002 | VERIFIED | resource-limited staged archive validation rejects traversal, links, expansion, size/count/hash/schema/asset failures with actionable typed errors (2026-07-11)
