# Performance baselines

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
scene-pipeline test uses three enabled detectors and proves that the unknown scene
evaluates only its one anchor, then the recognized scene evaluates the anchor plus one
contextual detector while permanently gating the unrelated third detector. Live state
reports evaluated, gated, and rate-throttled counts for direct profiling.

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
previously make an unknown base scene JSON-sufficient). The focused rerun passed all 18
unknown-scene cases. One private-profile failure remains: the `server_error` overlay
reuses guild attack-info anchors and co-activates with its specific overlay. The tool
reports this deterministic external-data regression but does not mutate the checksummed
private package.

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
