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
must be profiled separately before release. The current measurements do not justify
shared memory, DMA-BUF, GPU preprocessing, or a custom OBS path.
