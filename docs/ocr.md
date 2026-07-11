# OCR backend evaluation

Status: Tesseract selected, integrated, and verified for the post-release Phase 9
contract. Fixture accuracy limitations below remain visible tuning targets.

## Fixtures

`crates/vision/tests/fixtures/ocr` contains generated, redistributable 8-bit grayscale
HUD crops. They exercise English capitals, accented French text, downscaling, motion
blur, and a stroked/glowing presentation. The images contain no game footage and were
generated specifically for this project with Noto Sans Bold.

Regenerate the PNGs with ImageMagick and Noto Sans installed:

```bash
scripts/generate-ocr-fixtures.sh
```

## Reproduce the comparison

The benchmark intentionally keeps its Python dependencies outside the Rust product:

```bash
python -m venv /tmp/yash-ocr-bench
/tmp/yash-ocr-bench/bin/pip install -r scripts/requirements-ocr-benchmark.txt
/tmp/yash-ocr-bench/bin/python scripts/benchmark-ocr.py \
  --iterations 3 --output docs/ocr-benchmark.json
```

The reference run used Tesseract 5.5.2 and ONNX Runtime 1.27.0 on a Ryzen 7
5800X3D. Tesseract runs as a fresh CLI process in the comparison; RapidOCR runs its
complete PP-OCRv6 detection, classification, and recognition pipeline. Consequently,
the numbers compare deployable pipelines rather than isolated neural recognition.

| Backend | Exact cases | Mean confidence | Mean wall latency | Mean CPU | Process RSS |
|---|---:|---:|---:|---:|---:|
| Tesseract 5 | 4/5 | 0.912 | 95.2 ms | 119.8 ms | benchmark host 171 MB |
| RapidOCR/ONNX Runtime | 4/5 | 1.000 | 422.5 ms | 3482.5 ms | 420 MB |

The complete per-fixture result is recorded in `docs/ocr-benchmark.json`. Both
pipelines recognized the clean, scaled, motion-blurred, and glowing English fixtures.
Tesseract removed French accents; RapidOCR returned the two localized words in reverse
order. Those localization failures remain visible rather than being hidden by a
permissive metric.

## Decision

Tesseract is the initial backend because it tied exact accuracy on this small spike,
used materially less memory and latency, is already packaged on the reference Linux
environment, and has a maintained Rust binding. The product integrates it through the
project-owned typed detector boundary; no Tesseract type crosses into profiles,
engine, protocol, or GUI code.

OCR crops are evaluated on the image worker, never the capture callback or GUI render
thread. The detector compares the processed crop with the previous crop, reuses a
cached result while unchanged, and forces a configurable periodic refresh. Profile
validation bounds language/whitelist sizes, page segmentation mode, change threshold,
and refresh interval.

## Known limitations and next evidence

- Tesseract and Leptonica development libraries are currently source-build
  dependencies; packaging metadata still needs updating.
- The English language pack removes accents from the French localization fixture;
  profiles needing accent fidelity must select an installed matching language pack.
- Additional language packs require installation through the host distribution.
- Game-specific fonts still require their own replay datasets and acceptance thresholds.
