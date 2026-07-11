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
