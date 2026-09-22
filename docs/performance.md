# Performance baselines

## Application audit, 2026-09-20

The audit covers capture, vision, scheduling, live/replay publication, suite evaluation,
IPC, GUI refresh, profile persistence, archives, and catalog access. Fixes are applied in
descending impact order. Existing uncommitted profile and routing work is preserved.

1. Replay previously decoded up to 10,000 PNGs into a retained vector before processing.
   A 4096×4096 RGBA frame uses 64 MiB, so permitted input could exhaust memory. Image and
   synthetic replay now consume fallible frame iterators. Replay and suite PNGs share
   the bounded decoder, which checks dimensions before allocating or inflating pixels.
   Tests verify lazy decoding, release of consumed pixels, traversal rejection, and
   rejection of an oversized header before reading its truncated pixel stream. The
   daemon's 36 tests pass, including OCR/classifier replay and durable output tests.
2. Live and replay publication serialized and durably replaced the entire state after
   every detector observation, then queued another full copy for output routes. State
   is now published once after each analyzed frame. Transitions retain individual
   JSONL, subscription, and event-route delivery; state routes see complete frames.
   The 128-detector, 20-frame release benchmark fell from 1684.281 ms to 21.217 ms,
   about 79 times faster on this host. This synthetic result isolates publication
   overhead and does not predict OCR or real-game speed. The regression test verifies
   that no partial state reaches disk or `state.get`, all 128 observations survive,
   and a frame with no new observations causes no write.
3. Image analysis, replay, preview encoding, and detector setup ran directly on async
   control threads. They now use blocking workers with one shared foreground admission
   permit, including both suite entry points. Live analysis awaits one blocking frame
   job at a time and keeps the latest-frame input. A current-thread runtime test proves
   status remains responsive during blocked work and cancelling an awaiting task does
   not release the permit before native work finishes. All 38 daemon tests and strict
   workspace Clippy pass. This follows [Tokio's guidance for blocking CPU work](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html).
4. Capacity analysis could open and hash template files from the GUI render thread.
   The in-memory API now compares paths without filesystem access; only the explicit
   asset-root API hashes files. The GUI skips analysis when its panel is collapsed
   and retains one report until the draft content changes. Tests cover path-only versus
   content-aware reports, unsaved edits at the same revision, and profile switches.
   All 61 profile/GUI tests pass.
5. Template matching allocated two vectors and recomputed template statistics at every
   search position. It now retains count/mean/variance per template and scores directly
   from the image, without temporary pixel buffers. The existing release benchmark fell
   from 473.840 to 47.929 microseconds per evaluation. A differential test checks exact
   `f32` equality with the original algorithm over 4,096 masked, constant, empty-mask,
   single-pixel-mask, and varied search positions. All 22 vision tests pass. Malformed
   template dimensions are rejected at construction instead of reaching pixel indexing.
6. The GUI queued six more status requests every two seconds during long operations.
   Polls now retain at most one outstanding request of each kind, request and response
   channels hold at most 64 entries, and rejected manual requests produce a visible
   retry error. The test queues 1,000 polls, verifies one request, checks manual-command
   ordering and overload reporting, then confirms polling resumes after a response.
   Failed draft saves retry after a delay, failed detector tests release their pending
   flag, and unrelated successful polls do not erase the error message.
7. Packed capture frames were copied into a vector and then copied again into their
   shared buffer. Capture now allocates the shared buffer directly and normalizes
   channels in place. For 60 3840×2160 release-mode frames, RGBA fell from 2.383 to
   0.741 ms/frame and BGRx from 6.215 to 4.820 ms/frame. Existing stride, offset, channel,
   alpha, and short-buffer tests pass. Full-frame suite fixtures also skip allocation
   of an unused blank canvas before accepting the decoded pixels.
8. Disabled scenes and overlays still prewarmed and evaluated their recognition
   anchors. Scheduling, one-shot dependency selection, suggested anchor crops, and
   capacity accounting now exclude those layers. The five-detector regression retains
   its one-then-four evaluations even when the unused detector is referenced by both
   a disabled scene and a disabled overlay.
9. Profile loads parsed their active document a second time during lineage validation.
   They now compare the loaded revision against the lineage and any trashed document.
   Listing visits only canonical profile directories: an external backup can inform
   import rebasing without breaking the subsequent profile list. Tests retain the
   backup unchanged and reject a document whose ID differs from its directory.

The review also caught an authoring regression: deleting recognition evidence could
leave an empty `all` expression, making a scene or interaction target match without
evidence. Removal now disables affected scenes/overlays and hides affected targets until
their draft configuration is repaired. Profile and engine tests verify the behavior.

The 2026-09-20 audit's final automated validation was: formatting, strict workspace
Clippy, all 197 workspace tests, README claim checks, and documentation generation pass.
The two ignored benchmarks were run explicitly. An isolated release daemon passed the
available BlazBlue suite's
33 cases, 50 frames, and 126 assertions in 12.476 seconds, with a cold inventory cache.
During that run, 61 CLI status requests took 4.245 ms median and 5.730 ms maximum;
sampled daemon RSS peaked at 308.57 MiB. These measurements cover this workload only.
No fresh interactive portal/GUI capture test or full Queen Blade suite was run in this
audit; earlier evidence below is historical.

Reproduce the publication and capture benchmarks with:

```bash
cargo test --release -p yash-app-eventsd replay_publication_benchmark -- --ignored --nocapture
cargo test --release -p yash-app-events-capture-pw packed_capture_copy_benchmark -- --ignored --nocapture
```

## Follow-up hot-path audit, 2026-09-21

The follow-up pass re-audited the live frame loop, rule dispatch, detector ownership,
suite image flow, and output-route worker. Fixes were applied from highest to lowest
observed cost or frequency:

1. Live analysis now rejects frames above the configured 1–10 FPS rate before submitting
   a blocking worker job. The latest-frame poll runs at half the configured analysis
   interval and skips missed timer ticks, so the worker does not wake at 200 Hz for a
   10 FPS pipeline.
2. Collection admission is checked before building observation evidence or cloning
   collection observations. Disabled, throttled, and already-writing collectors now
   avoid all full-frame thumbnail work and per-observation evidence copies.
3. Rule dispatch is indexed by affected element IDs. Composite rule evaluation folds
   its bounded conditions without a temporary vector or synthetic observation allocation.
   Scene overlay duplicate suppression uses a highest-priority target index, and the
   live active-element set is retained between frames.
4. Derived observations consume the typed latest-observation map directly instead of
   serializing to JSON and deserializing back through the composition path. Spatial suite
   cases decode each PNG once and share its pixel storage with the analysis frame.
5. Detector crops now use ownership-taking preprocessing and move stateful image buffers
   into detector state without an unconditional clone. Color-bar scratch vectors and
   live overlay IDs are reused between evaluations.
6. The output-route worker caches validated per-profile route vectors and refreshes them
   on route list/set/enable/remove/install operations, removing repeated route-config
   file reads and JSON parsing from state publication.

Targeted daemon, engine, and vision tests passed after each relevant change. The final
workspace run passed 198 tests with 2 ignored, and strict workspace Clippy passed. A fresh
release detector sample on this host measured color bar 5.441 µs, template 47.371 µs, and
region change 5.215 µs per evaluation; these are workload samples, not supported-system
targets. No new interactive portal or native GUI capture run was performed.

## Additional hot-path audit fixes, 2026-09-21

The audit continued through the remaining high-frequency publication, route, resolver,
and GUI paths:

1. Latest daemon state is retained as one immutable typed `Arc<StateSnapshot>`. Each
   analyzed frame no longer converts that snapshot into a second full JSON value just
   for `state.get` and the route queue; readers clone the `Arc` and serialize only at
   the RPC boundary. The route worker shares the same snapshot instead of receiving a
   deep copy.
2. `state.json` still uses same-directory atomic replacement, but its per-frame write
   now flushes userspace buffers without forcing a filesystem sync barrier. Event-log
   flush/rotation and atomic-replace output routes retain their existing durability
   policies.
3. Full output routes no longer serialize an unused `{kind,event,state}` context before
   serializing the selected event or state. State routes render once for deduplication
   and reuse that rendered payload for delivery.
4. Scene and overlay resolution builds stable profile indexes once and precomputes
   caller-hint ranks, removing repeated linear profile scans from sort and target
   suppression paths.
5. The GUI caches pretty-printed scene context and event state until the next state
   response, indexes profile identities for the live observation panel, avoids per-frame
   route/recipe/collection vector clones, and uses a slower repaint interval while idle.
6. The route cache mutex is no longer held during sink execution. Cached profiles with
   no enabled route matching the job type skip queue submission, and rendered state
   payloads for removed routes are pruned from the worker's deduplication cache.

Targeted output, daemon, engine, and GUI tests passed after these changes; strict
workspace Clippy also passes. These fixes were verified functionally and are not
represented as new benchmark targets without a representative interactive workload.

## Final hot-path audit, 2026-09-21

A further audit included the uncommitted application changes and checked the remaining
suite, live, status, output, resolver, and GUI repaint paths:

1. One-shot and spatial-suite PNG analysis now hashes the bytes already read for decoding
   instead of opening the fixture a second time. One-shot scene resolution evaluates one
   frame once, so an N-of-M requirement cannot be fabricated by replaying the same image;
   suite pipeline construction also borrows the loaded profile rather than cloning the
   complete document for each case. Canonical suite roots are reused while per-image
   canonicalization and traversal checks remain in place.
2. Scene-aware live detector scheduling precomputes enabled-processor and anchor indexes,
   processes only active contextual indexes, and releases only previously active contextual
   processors. Detector order and lazy construction behavior remain unchanged.
3. Status process accounting checks its 500 ms cache before reading `/proc`, and parses
   CPU ticks without allocating a temporary field vector. A parser regression test covers
   command names containing `)` and malformed input.
4. The output worker now validates a route once before rendering/delivery, while public
   untrusted helpers retain their validation guarantees. Atomic replace routes still use
   same-directory temporary-file replacement, but flush userspace buffers without forcing
   a file and directory sync barrier on every delivery.
5. The GUI caches profile-derived observation names, detector labels, regions, and derived
   inputs. The live observation panel still sorts current observations and reads current
   values each repaint, but no longer rebuilds profile indexes on every egui frame.

The final verification for this pass is formatting, strict workspace Clippy, all-feature
workspace tests, README claim checks, and workspace documentation generation. No fresh
portal or native GUI capture run was performed, and no GPU/shared-memory or concurrent
suite change was made without a representative workload.

## Deterministic detector microbenchmark

Recorded 2026-07-11 on an AMD Ryzen 7 5800X3D (8 cores / 16 threads), using
`rustc 1.95.0`, an optimized build, a 100×20 RGBA fixture, and 10,000 sequential
evaluations per detector:

| Detector | Microseconds per evaluation |
|---|---:|
| Color bar | 5.367 |
| Template (5×5 sliding match) | 516.306 |
| Region change | 5.618 |

Reproduce with:

```bash
cargo run --release -p yash-app-events-vision --example detector_benchmark
```

This is a regression baseline, not a supported-system performance claim. Portal
capture, copying, multiple configured regions, GUI preview, and end-to-end CPU use
still require interactive portal profiling. The current measurements do not justify
shared memory, DMA-BUF, GPU preprocessing, or a custom OBS path.

## Daemon scheduling baselines

On the same reference host, the release daemon with capture stopped and no preview
client consumed 0 scheduler CPU ticks over a two-second `/proc/<pid>/stat` window
(`CLK_TCK=100`). It has no periodic image task in that state. Reproduce by starting
the release daemon in an empty XDG tree, sampling `utime + stime`, waiting two seconds,
sampling again, and shutting it down through the CLI.

The CI-safe live-worker test injects 60 timestamped frames per second into the same
latest-frame slot used by PipeWire, permits no more than 10 detector evaluations per
second, records 59 replacements, and emits the expected transition without backlog.

Schema-2 scheduling adds no frame queue. Resolver histories are capped at 32 samples,
scene/overlay candidate counts are bounded by profile validation, and N-of-M history
advances only when an anchor detector produces a newly scheduled observation. The
scene-pipeline test uses five enabled detectors and proves that the unknown scene
evaluates only its one anchor, then the recognized scene evaluates the anchor plus its
ordinary, required, and target-visibility contextual dependencies while permanently
gating the unrelated fifth detector. Live state reports evaluated, gated, and
rate-throttled counts for direct profiling.

## External suite phase timings

Use an optimized daemon and request opt-in suite timings without changing evaluation
semantics:

```bash
cargo build --release --workspace
yash-eventsctl --json --timeout-ms 600000 suite evaluate /path/to/package --timings
```

The result reports millisecond wall times for SHA-256 inventory verification, profile
load/validation, all case work, and result serialization, followed by at most ten
slowest case IDs. The ordinary response omits timings for compatibility. Compare
multiple idle-daemon runs; millisecond values are intentionally not correctness gates.

The 202-case Queen Blade workload on 2026-08-18 demonstrated the original failure mode:
the synchronous daemon exceeded ten minutes, blocked a five-second `status` request, and
grew beyond 1.4 GiB RSS before the isolated baseline was stopped. Its 12 GiB package
contains 1,041 pinned files and a 512-element profile. With first-class operations, a
focused Coin Trial run passed 10/10 assertions in 5.29 seconds: inventory verification
was 296 ms, profile load 10 ms, serialization below 1 ms, and case work 4.85 seconds.
This identifies detector/OCR evaluation as the dominant phase. Control `status` remained
responsive in about 4 ms during the new background operation, and cancellation completed
at the next case boundary with the active case/phase retained.

The first observable complete operation peaked above 1.64 GiB RSS. Suite execution is
therefore limited to one operation/worker at a time under a provisional 2 GiB daemon
budget; case concurrency is intentionally rejected until detector/OCR memory falls.

That operation completed all 202 cases in 918.772 seconds: case work consumed
918.772 seconds versus 170 ms inventory, 7 ms profile load, and 107 ms serialization;
the slowest case was 6.753 seconds. It found 13 correctness failures. Twelve unknown-scene
handoff failures revealed and fixed a generic fail-open condition (an overlay match could
previously make an unknown base scene JSON-sufficient), and the focused rerun passed all
18 unknown-scene cases. The remaining private-data failure was corrected by adding the
existing `fast_btl_adventure_title_anchor` to the `server_error` overlay recognition;
the anchor is strong on the server-error frame and absent from the guild attack-info
frame. A fresh release run then passed 202/202 cases and 2,129/2,129 assertions in
958.134 seconds (inventory 192 ms, profile load 10 ms, serialization 110 ms; slowest
case 4.917 seconds). The corrected package profile and suite hashes are recorded in
the private Queen Blade evidence log.

Inventory hashing now streams through 64 KiB rather than reading each large package file
into one allocation. A bounded cache reuses verification only when the package root,
manifest SHA-256, and every pinned file's path/size/mtime still match; cold runs remain
available with `--no-cache`.

## Profile capacity decision

Run:

```bash
cargo run --release -p yash-app-events-profile --example profile_capacity_benchmark
yash-eventsctl --json profile analyze-capacity /path/to/profile.json
```

Generated mixed profiles at 128/256/384/512 elements parsed in 0.24/0.33/0.52/0.66 ms
and validated in 0.05/0.09/0.12/0.19 ms on the reference host. Serialization remained
below 0.27 ms. Generated 768/1024 documents remained cheap to parse but were correctly
rejected by the current bound. The real 512-element workload is already dominated by
runtime detector cost and exceeds one GiB RSS, so the limit remains **512**. Raising it
would enlarge worst-case runtime and authoring complexity without solving the measured
bottleneck. Capacity warnings and consolidation analysis are the selected remedy.

The capacity analyzer now reports the unique enabled anchors evaluated before scene/overlay
selection, including weighted detector cost and family breakdown. It also recognizes template
duplicates by content rather than only by path, so copied or renamed assets appear as
consolidation candidates. Portable exports omit template files unreferenced by the current
profile; other allowed portable files and source directories are left untouched. The daemon's scene-aware pipeline now keeps those
anchors resident, constructs contextual processors only after a matching scene/overlay is
selected, and releases inactive contextual processors. The no-context/active-context regression
test proves that this changes construction and residency without changing detector results,
including required and target-visibility dependencies.

For unrelated HUD families, a profile bundle bounds the second stage to one selected member:
the request pays for the small router plus the chosen profile, never for every member. The
router and each member retain the existing 512-element admission bound; the bundle manifest is
limited to 256 KiB and 32 members. Revision pins and a shared request timeout prevent a stale
or unexpectedly expensive member from silently extending an analysis indefinitely.
