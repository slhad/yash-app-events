# Current implementation plan

`SPECS.md` is the source of truth. This file records work that is active or still needs
verification. Completed design and implementation details belong in Git history, tests, and
the specification evidence index.

## Current state

The first usable Linux release and its post-release milestones are complete. The repository
implements portal and PipeWire capture, deterministic and OCR/model detectors, temporal rules,
portable profiles, replay and external suites, durable and live outputs, the CLI, the native
GUI, scene-aware spatial analysis, profile bundles, transition context, capacity diagnostics,
and the public profile catalog.

The 2026-09-20 application audit fixed the largest measured performance problems:

- replay decodes one bounded frame at a time;
- state snapshots publish once per analyzed frame;
- image work runs outside async control threads with bounded admission;
- template scoring avoids per-position allocations;
- capture avoids a second full-frame copy;
- GUI polling and capacity analysis remain bounded;
- disabled layers do not keep recognition anchors active;
- profile listing and revision checks avoid redundant or unsafe work.

A 2026-09-21 follow-up pass also rate-gates live work before worker submission, admits
collection evidence before frame-wide copies, indexes rule dispatch, keeps typed derived
observations and detector scratch storage in memory, shares spatial-suite frame pixels,
and caches output routes. The canonical evidence is recorded in the dated follow-up
section of `docs/performance.md`.

The final 2026-09-21 hot-path audit also removed duplicate one-shot PNG reads and temporal
sample replay, indexed scene-aware processor scheduling, moved the status `/proc` cache
check before filesystem reads, avoided duplicate route validation and per-delivery sync
barriers, reused canonical suite roots, and cached the GUI observation profile index.
The canonical evidence is in `SPECS.md` and `docs/performance.md`. The current tree passes
200 workspace tests and strict workspace Clippy. The available 33-case, 126-assertion game
suite and interactive portal/GUI runs remain dated evidence; no fresh interactive run was
part of this audit.

## Active work

No product milestone is currently scheduled.

Before a release that claims fresh desktop integration evidence, repeat the applicable portal
and native GUI smoke tests in `docs/capture-smoke.md` and `docs/gui-acceptance-report.md`.
Historical Hyprland evidence remains valid for the code paths it covered, but it is not a
substitute for a new environment-specific acceptance run.

Ideas such as additional capture backends, GPU transfer paths, remote control, and new output
adapters remain unscheduled in `ROADMAP.md`.

## Change sequence

For each implementation change:

1. Identify the relevant `SPEC-*` requirements.
2. Add or update tests that prove the intended behavior.
3. Implement the smallest complete change across affected boundaries.
4. Run targeted tests, then the workspace quality gates.
5. Update specification evidence after the required proof passes.
6. Update this file only when active ordering or scope changes.
7. Update `README.md` only for verified user-visible behavior.

Do not mark work complete because code exists. Use the acceptance evidence required by
`SPECS.md`.

## Required verification

The minimum automated checks are:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
bash scripts/check-readme-claims.sh
cargo doc --workspace --no-deps
```

Run schema compatibility, protocol, replay, archive safety, benchmark, and live desktop tests
when the affected requirement calls for them. Record benchmark commands and hardware limits in
`docs/performance.md` rather than this plan.

## Release gate

A release candidate must have:

- no unresolved required specification entries;
- passing canonical checks;
- passing compatibility and security tests for changed external contracts;
- current user documentation;
- measured evidence for performance claims;
- an explicit note for any hardware or interactive check that was not repeated.
