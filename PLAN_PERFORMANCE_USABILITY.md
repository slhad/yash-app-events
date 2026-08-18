# Performance, Profiling, Reliability, and Usability Plan

Status: proposed investigation and implementation plan, based on the Queen Blade
schema-2 profile reaching the 512-element validation limit and a 202-case external
suite exceeding a 10-minute daemon RPC deadline on 2026-08-18.

`SPECS.md` remains normative. This document does not change profile/protocol contracts
or authorize raising resource limits; any such change must first update the relevant
`SPEC-*` requirements and compatibility tests.

## Outcomes

1. Explain where analysis time, CPU, memory, and I/O are spent for a full profile and
   external suite.
2. Make long operations observable, cancellable, and diagnosable instead of appearing
   hung behind one silent RPC.
3. Make focused profile authoring and regression checks fast while preserving a trusted
   complete-suite gate.
4. Explain profile capacity before validation fails and provide actionable ways to
   consolidate or remove redundant detector elements.
5. Establish measured criteria for retaining or changing the current bounds.

## Current evidence and hypotheses

- `crates/profile/src/lib.rs` validates at most 512 detector elements, 128 scenes,
  128 overlays, 256 elements per layer, and 512 total interaction targets. The limits
  satisfy the bounded-scene requirement in `SPEC-SCENE-003`, but their rationale and
  measured worst-case costs are not recorded.
- The Queen Blade packaged profile is exactly at 512 elements. A Coin Trial change was
  implemented without adding elements by reusing existing observations, demonstrating
  that consolidation is often preferable to increasing the limit.
- A focused Queen Blade case completed successfully with 10/10 assertions. The complete
  202-case suite did not return before a 600,000 ms client deadline.
- `evaluate_regression_suite` currently verifies every inventory file and evaluates
  cases sequentially inside one synchronous daemon request. The response contains only
  final results, so callers cannot distinguish hashing, decoding, detector work, a slow
  case, daemon contention, or deadlock while waiting.
- Existing status metrics expose aggregate detector scheduling and latency, but external
  suite results do not provide phase, detector, case, or frame timings.

These are hypotheses until the instrumentation below identifies the actual dominant
cost. Do not introduce concurrency, caching, or higher limits based only on suite wall
time.

## Phase 0 — Reproduce and preserve evidence

Relevant requirements: `SPEC-OBS-001`, `SPEC-OBS-002`, `SPEC-REPLAY-005`,
`SPEC-SCENE-003`, `SPEC-PERF-001`, and `SPEC-PERF-003`.

- Add a documented command that records binary version, daemon instance, profile hash,
  suite hash, case/frame/file counts, host CPU, release/debug mode, and requested timeout.
- Reproduce the complete Queen Blade suite from an idle daemon at least three times.
- Separately measure profile load/validation, inventory verification, case JSON loading,
  image read/decode, detector preparation, scene resolution, contextual detection,
  assertions, and result serialization.
- Capture process CPU, peak RSS, I/O bytes, and daemon responsiveness to `status` during
  the run. Confirm whether the daemon request executor blocks unrelated control calls.
- Add a watchdog diagnostic that records the currently running suite phase and case ID
  without retaining fixture pixels.
- Keep one small synthetic suite, one medium public/private-safe suite, and the full
  Queen suite as three reproducible workload classes.

Exit gate: three runs identify the dominant phase(s), distinguish CPU work from waiting,
and produce enough evidence to reproduce the timeout without relying on terminal output.

## Phase 1 — First-class profiling and progress

- Introduce stable timing fields for suite phases, cases, frames, and detector families.
  Report wall time plus CPU-relevant work counters; do not make tests assert noisy exact
  durations.
- Add detector scheduling counts per case/frame: evaluated, gated, throttled, cache hit,
  OCR calls, template comparisons, and decoded image bytes.
- Emit bounded progress notifications over the existing protocol with operation ID,
  phase, completed/total cases and frames, current case ID, elapsed time, and estimated
  remaining work only when enough samples exist.
- Add `suite status <operation-id>` and `suite cancel <operation-id>`. Cancellation must
  be cooperative at inventory, case, and frame boundaries and must leave no partial
  profile or suite writes.
- Return a structured timeout/cancellation result containing the last completed phase and
  case instead of only `daemon request timed out`.
- Add `--timings`, `--progress`, and `--output <path>` CLI options. JSON mode must remain
  machine-readable; human progress belongs on stderr.
- Export timing summaries in privacy-bounded diagnostics without fixture images or local
  account data.

Exit gate: a deliberately slow test visibly advances, remains cancellable, leaves the
daemon responsive, and reports the exact phase/case where it stopped.

## Phase 2 — Faster authoring feedback

- Make focused execution a supported CLI feature rather than requiring a generated
  one-case suite: filters for exact case ID, category, changed fixture, scene, overlay,
  and detector/element ID.
- Add `--fail-fast` for authoring and CI triage while keeping aggregate mode as default.
- Add a two-tier workflow:
  1. profile validation plus impacted/focused cases for rapid iteration;
  2. the complete checksummed suite as the installation/publication gate.
- Produce a machine-readable impact explanation showing why each selected case is needed
  (changed element, referenced scene/overlay/target, shared template asset, or explicit
  category selection). Uncertain dependency mapping must include the case rather than
  silently skipping it.
- Cache only immutable work keyed by content hashes: verified inventory entries, decoded
  PNGs within a bounded operation cache, preprocessed deterministic templates, and model
  initialization. Include profile revision/hash and detector configuration in keys.
- Report cold and warm timings separately. Provide `--no-cache` and bounded cache metrics
  so correctness can always be checked from a cold path.

Exit gate: the focused Queen Coin Trial check is one explicit command, completes within
a documented target on the reference host, and every focused run prints the complete
suite command still required before installation.

## Phase 3 — Suite engine isolation and throughput

- Move long suite execution behind a daemon-owned operation manager so one request does
  not monopolize a connection handler or prevent status/cancel requests.
- Separate inventory validation from evaluation and reuse a verified inventory only when
  root identity, manifest hash, file metadata, and content hashes prove it unchanged.
- Profile image decoding and detector initialization before adding parallelism.
- If measurements justify concurrency, parallelize independent cases with a small
  configurable worker bound. Preserve deterministic result ordering by suite case order.
- Keep frames within one temporal case sequential. Do not share mutable detector/rule
  history across cases.
- Define CPU and memory budgets. Reduce worker count under OCR/model-heavy workloads or
  memory pressure instead of allowing unbounded parallel decoding.
- Add deadlock/stall detection based on lack of completed work plus task state, not merely
  elapsed wall time. Capture a bounded diagnostic snapshot before aborting a stalled run.

Exit gate: complete-suite wall time improves materially on the reference workload, peak
RSS remains within the documented budget, results are byte-stable across worker counts,
and status remains responsive throughout.

## Phase 4 — Profile capacity analysis and authoring usability

- Expose all limits through shared profile metadata/API rather than only hard-coded error
  strings. CLI and GUI should show used/maximum counts for elements, scenes, overlays,
  per-layer references, recognition conditions, and interaction targets.
- Add warnings at configurable percentages (for example 80%, 90%, and 100%) with the
  largest scenes/layers and most expensive detector families.
- Add `profile analyze-capacity` that reports:
  - unused or unreachable elements;
  - duplicate detector configurations/regions/assets;
  - elements referenced only by disabled layers;
  - repeated templates that can use one multi-template detector;
  - observations suitable for derived composition instead of another detector;
  - global elements that could be contextual;
  - estimated per-frame detector cost by active scene.
- Keep suggestions read-only initially. Any automated consolidation must preserve stable
  IDs/references or provide an explicit reviewed migration and full regression proof.
- Improve validation errors to say `512/512 detector elements used`, identify the limit
  source, and point to capacity analysis rather than merely rejecting element 513.
- Document patterns for large profiles: shared anchors, contextual scheduling, derived
  observations, multi-template detectors, and when separate profiles are preferable.

Exit gate: a profile author sees capacity before failure and receives at least one
actionable, regression-safe consolidation path for a deliberately redundant fixture.

## Phase 5 — Decide whether 512 should change

Do not raise the limit merely because one profile reached it. Build generated profiles at
128, 256, 384, 512, 768, and 1024 elements with representative mixes of cheap templates,
large templates, OCR, and scene-gated contextual detectors.

For each size, measure:

- profile parse/validation/load time and memory;
- cold/warm one-shot analysis latency;
- recognized-scene and unknown-scene worst cases;
- full-suite throughput and peak RSS;
- GUI profile editing/serialization responsiveness;
- archive size/import validation and diagnostic output size.

Decision options:

1. Retain 512 and improve consolidation/profile partitioning if worst-case or authoring
   costs are already high.
2. Raise conservatively (for example to 768) only if measured budgets pass and schema,
   archive, GUI, daemon, and suite stress tests remain bounded.
3. Replace one global constant with validated cost budgets only if a simple count is
   proven too crude and the new model remains deterministic and understandable.

Record the rationale in `SPECS.md` and `docs/performance.md`. A changed bound requires
compatibility tests, adversarial resource tests, release notes, and verification against
at least the Queen Blade and BlazBlue profiles.

## Phase 6 — Regression and operational gates

- Add CI-safe performance trend tests using generous regression thresholds and store raw
  benchmark artifacts; avoid flaky absolute microsecond assertions in ordinary unit tests.
- Add release-mode benchmark commands for profile validation, one-shot scene analysis,
  cold/warm suite execution, OCR, and template-heavy workloads.
- Fail CI on functional regressions. Report performance drift first; promote a metric to
  a blocking budget only after its variance is characterized on controlled runners.
- Add soak tests for repeated suite operations, cancellation, client disconnect, daemon
  restart, cache eviction, and concurrent status clients.
- Verify no suite timeout leaves an orphaned operation consuming CPU/memory.
- Update `docs/performance.md`, `docs/replay.md`, protocol docs, CLI man page, and GUI help
  as each user-visible capability is verified.

Exit gate: maintainers can reproduce baselines, compare a change with the baseline, find
the slow case/detector, and recover cleanly from timeout or cancellation.

## Recommended implementation order

1. Phase timings and current-case diagnostics.
2. Asynchronous operation/progress/status/cancel protocol.
3. Native focused filters and fail-fast.
4. Content-hash caches with cold-path verification.
5. Bounded case parallelism if profiling justifies it.
6. Capacity UI/reporting and consolidation analysis.
7. Generated limit benchmarks and the evidence-based 512 decision.

This order improves evidence and daily usability before making performance-sensitive or
compatibility-sensitive architectural changes.

## Verification matrix

| Area | Required proof |
|---|---|
| Correctness | Existing workspace tests plus byte-stable suite results before/after optimization |
| Responsiveness | `status` and `suite status/cancel` respond during a deliberately slow run |
| Performance | Three cold and three warm release runs for small, medium, and Queen workloads |
| Memory | Peak RSS and bounded cache/worker accounting under template/OCR-heavy suites |
| Determinism | Same ordered results for worker counts 1 and N and cache enabled/disabled |
| Capacity | Generated profiles at each target size and actionable 80/90/100% diagnostics |
| Safety | No fixture persistence beyond explicit suite inputs; no profile writes during evaluation |
| Recovery | Timeout, cancellation, disconnect, and daemon restart leave no orphaned work |

## First executable slice

The first implementation slice should remain small:

1. Add a `SuiteTimingSummary` containing inventory, profile-load, total-case, and total
   serialization durations plus the slowest ten case IDs and durations.
2. Instrument `evaluate_regression_suite` and `evaluate_suite_case` without changing
   scheduling or result semantics.
3. Include timings only when explicitly requested so existing protocol golden output
   remains compatible.
4. Add deterministic tests using relative/order assertions rather than exact elapsed
   values.
5. Re-run the 202-case Queen suite and use the result to choose the second slice.

This slice will tell us whether hashing, decoding, detector work, or daemon scheduling is
the first optimization target instead of guessing.
