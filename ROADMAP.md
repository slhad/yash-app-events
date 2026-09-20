# Roadmap

`SPECS.md` defines committed behavior. Items in this file are ideas, not promises. Promote an
item into `SPECS.md` and `PLAN.md` only after a concrete use case, compatibility review, and
acceptance criteria exist.

## Completed milestones

- First usable Linux and Wayland release, 2026-07-11.
- Complete temporal rule language, OCR, bounded diagnostic exports, and generic ONNX image
  classification, 2026-07-11.
- Profile-scoped output routes and portable inert recipes, 2026-07-17.
- Public profile catalog with reviewed, versioned packages, 2026-07-18.
- Scene-aware analysis, spatial JSON, crop-first escalation, and optional isolated-Wayland
  adapter, 2026-08-03.
- Long-suite operations, profiling, cancellation, focused evaluation, bounded inventory cache,
  and profile capacity diagnostics, 2026-08-18 through 2026-09-04.
- Lazy contextual detectors, profile-bundle routing, revision-lineage hardening, and bounded
  transition context, 2026-09-04 through 2026-09-15.
- Full application performance audit, 2026-09-20.

The specification evidence index and `docs/performance.md` contain the measurements and test
results. Git history retains the implementation plans that produced these milestones.

## Unscheduled candidates

- GNOME and KDE portal acceptance on additional distributions.
- Shared-memory preview after transfer profiling shows a material benefit.
- DMA-BUF or GPU preprocessing after CPU and copy measurements justify the complexity.
- OBS plugin or shared-texture integration.
- X11 capture.
- Windows Graphics Capture.
- Authenticated remote control. Local Unix control remains the default security boundary.
- MQTT, Home Assistant, and webhook output adapters.
- Multi-source simultaneous capture.
- Bounded case parallelism after detector memory falls below the current single-worker budget.

## Promotion gate

Before scheduling a candidate:

1. Describe the user problem and why existing behavior is insufficient.
2. Define security, privacy, compatibility, and resource limits.
3. Add normative requirements and acceptance evidence to `SPECS.md`.
4. Add an ordered implementation slice to `PLAN.md`.
5. Measure the current path before choosing a new dependency or architecture.

Keep a candidate deferred when its measured benefit does not cover its runtime, maintenance, or
distribution cost.
