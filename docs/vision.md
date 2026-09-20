# Deterministic vision implementation decision

Phase 4 keeps image conversion, preprocessing, color-bar measurement, normalized
template matching, and region-change measurement in project-owned safe Rust types.
No OpenCV types cross a crate boundary, and OpenCV is not currently linked.

The original plan proposed OpenCV as an implementation dependency. The 2026-07-11
100×20 release-mode baseline measured approximately 5.4 µs for color bars, 516 µs for
a sliding 5×5 template, and 5.6 µs for region change on the documented reference CPU.
After removing per-position allocations and retaining template statistics, the
2026-09-20 audit measured the same template workload at approximately 48 µs. See the
[performance evidence](performance.md) for both baselines and reproduction commands.
These results, along with deterministic tests for stride, scale, noise, brightness,
masks, preprocessing, and change baselines, do not establish a benefit that justifies
the native dependency and packaging cost. The project-owned `Detector` and `Frame`
boundaries permit a later OpenCV-backed implementation if replay profiling shows a
material bottleneck.

Portable template and mask assets are JSON-serialized `GrayImage` and row-major boolean
arrays. Their internal encoding may evolve through explicit schema migration; versioned
profile paths and archive integrity are the external contract.
