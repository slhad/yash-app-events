# Post-release Roadmap

This roadmap begins after completion of the first usable Linux release recorded in
`GOAL.md`. `SPECS.md` remains normative. Before implementation begins on a milestone,
promote its requirements from deferred or future scope into explicit acceptance
criteria in `SPECS.md`. Do not claim support until those criteria have evidence.

## Progress

- R0 — release handoff and documentation: complete (2026-07-11).
- R1 — complete event-rule language: complete (2026-07-11). Typed predicates,
  stable duration, bounded composition, initial/updated transitions, schema compatibility,
  GUI authoring, and common live/image-replay coordination pass.
- R2 — OCR detector: complete (2026-07-11). Fixtures, Tesseract/ONNX benchmark,
  backend decision, typed Tesseract detector, change-triggered scheduling,
  profile/daemon/GUI wiring, frozen diagnostics, and image replay event pass.
- R3 — privacy-bounded diagnostic bundle: complete (2026-07-11). Protocol, CLI, and
  GUI provide review-before-export; recursive redaction, explicit frozen-region crops,
  limits, atomic ZIP output, and adversarial tests pass.
- R4 — generic image classifier: complete for the validated generic workflow
  (2026-07-11). Generated dataset/model, SHA and resource validation, bounded ONNX CPU
  inference, scheduling/cache, GUI diagnostics/configuration, image replay, temporal
  event integration, and recorded benchmark pass. Real game-specific models remain
  profile assets requiring their own datasets and evidence.
- R5 — performance and platform candidates: uncommitted; promote only from measured
  need and a concrete use case.

## R1 — Complete event-rule language

Implement the remaining behavior anticipated by `SPEC-EVENT-002` and
`SPEC-EVENT-003` across profile schema, validation, engine, protocol, CLI/GUI,
replay, and output:

1. Boolean appearance/disappearance.
2. String equality and substring matching.
3. Stable-duration evidence.
4. Conjunction and disjunction of observations.
5. Explicit, rate-limited `updated` transitions.
6. Configurable initial-state transition emission after startup.

Exit gate:

- Every primitive has schema round-trip, validation, deterministic engine, replay,
  and GUI authoring coverage.
- Composition has bounded history and evaluation cost and cannot introduce cycles.
- Transition golden tests cover initial establishment, entered, updated, and left.
- Existing schema-v1 profiles and protocol-v1 clients remain compatible, or a
  versioned migration is supplied.

## R2 — OCR detector

Complete Phase 9 from `PLAN.md`:

1. Add legally redistributable fixtures covering representative HUD fonts,
   localization, scale, animation, glow, and background variation.
2. Benchmark Tesseract and an ONNX recognition pipeline for field/event accuracy,
   latency, confidence calibration, CPU, and memory.
3. Record the backend decision and distribution consequences.
4. Integrate the selected backend behind the detector boundary.
5. Add bounded scheduling, including region-change-triggered evaluation where useful.
6. Add profile validation, GUI configuration, preprocessing preview, replay metrics,
   and regression tests.

Exit gate:

- The backend choice is justified by reproducible benchmark evidence.
- OCR never blocks capture or the GUI render thread and has bounded resource use.
- OCR observations use the common temporal-rule and output paths.
- Localization, scaling, animation, and glow regressions pass.

## R3 — Diagnostic bundle

Promote `SPEC-OBS-003` and implement an explicit export workflow containing redacted
logs, configuration, metrics, and only user-selected crops.

Exit gate:

- The UI presents the exact included files and a privacy warning before export.
- Portal tokens, machine-local bindings, secrets, and full screenshots are excluded
  by default and tested with adversarial fixtures.
- Per-file, file-count, and total-size limits are enforced.
- Bundle creation is atomic and does not run on the GUI render thread.

## R4 — Generic image classifier

Promote `SPEC-DET-006` only after representative, redistributable replay datasets and
deterministic baselines exist. Then add validated model assets, bounded inference,
GUI configuration and diagnostics, replay evaluation, and temporal-rule integration.

Exit gate:

- Dataset, accuracy, latency, confidence, CPU, and memory evidence justify the model.
- Untrusted models and inputs have explicit validation and resource limits.
- Classifier failures produce `unknown` or `error`, never fabricated negatives.
- Installation and packaging of the selected runtime are verified.

## R5 — Candidates requiring promotion

The following are not scheduled commitments:

- GNOME and KDE portal acceptance and broader distribution packaging.
- Shared-memory preview, DMA-BUF, or GPU preprocessing.
- OBS plugin/shared-texture integration.
- X11 and Windows capture backends.
- Authenticated remote WebSocket control.
- MQTT, Home Assistant, and webhook adapters.
- Multi-source simultaneous capture.

Promote a candidate only after documenting its user need, security boundary,
compatibility impact, measurements, acceptance criteria, and ordering relative to the
active milestone.

## Verification discipline

For every milestone:

1. Update `SPECS.md` before claiming the expanded behavior.
2. Preserve bounded capture, subscriptions, scheduling, and image/model processing.
3. Add schema/protocol golden and migration tests for external-contract changes.
4. Run formatting, strict Clippy, workspace tests, targeted replay/benchmark tests,
   README claim checks, and documentation generation.
5. Record status and concrete evidence in `SPECS.md` and this roadmap.
