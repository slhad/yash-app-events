#!/usr/bin/env python3
"""Reproducible Tesseract versus ONNX Runtime OCR fixture benchmark."""

import argparse
import json
import resource
import statistics
import subprocess
import time
from pathlib import Path

import psutil
from rapidocr import RapidOCR


EXPECTED = {
    "victory.png": "VICTORY",
    "localized.png": "NIVEAU ÉTÉ",
    "scaled.png": "VICTORY",
    "animated.png": "VICTORY",
    "glow.png": "VICTORY",
}


def normalized(text: str) -> str:
    return " ".join(text.upper().split())


def tesseract_once(path: Path) -> tuple[str, float]:
    result = subprocess.run(
        ["tesseract", str(path), "stdout", "-l", "eng", "--psm", "7", "tsv"],
        check=True,
        capture_output=True,
        text=True,
    )
    words = []
    confidences = []
    for line in result.stdout.splitlines()[1:]:
        columns = line.split("\t")
        if len(columns) == 12 and columns[11].strip():
            words.append(columns[11].strip())
            confidence = float(columns[10])
            if confidence >= 0:
                confidences.append(confidence / 100.0)
    return " ".join(words), statistics.fmean(confidences) if confidences else 0.0


def rapid_once(engine: RapidOCR, path: Path) -> tuple[str, float]:
    result = engine(str(path))
    text = " ".join(result.txts or ())
    confidence = statistics.fmean(result.scores) if result.scores else 0.0
    return text, confidence


def benchmark(name: str, run, fixtures: Path, iterations: int) -> dict:
    cases = []
    for filename, expected in EXPECTED.items():
        path = fixtures / filename
        run(path)
        latencies = []
        cpu_times = []
        outputs = []
        confidences = []
        for _ in range(iterations):
            child_before = resource.getrusage(resource.RUSAGE_CHILDREN)
            cpu_before = time.process_time()
            started = time.perf_counter()
            text, confidence = run(path)
            latencies.append((time.perf_counter() - started) * 1000.0)
            child_after = resource.getrusage(resource.RUSAGE_CHILDREN)
            child_cpu = (child_after.ru_utime + child_after.ru_stime) - (
                child_before.ru_utime + child_before.ru_stime
            )
            cpu_times.append((time.process_time() - cpu_before + child_cpu) * 1000.0)
            outputs.append(text)
            confidences.append(confidence)
        chosen = max(set(outputs), key=outputs.count)
        cases.append(
            {
                "fixture": filename,
                "expected": expected,
                "observed": chosen,
                "exact": normalized(chosen) == normalized(expected),
                "mean_confidence": statistics.fmean(confidences),
                "mean_latency_ms": statistics.fmean(latencies),
                "mean_cpu_ms": statistics.fmean(cpu_times),
            }
        )
    process = psutil.Process()
    return {
        "backend": name,
        "cases": cases,
        "exact_accuracy": sum(case["exact"] for case in cases) / len(cases),
        "mean_confidence": statistics.fmean(case["mean_confidence"] for case in cases),
        "mean_latency_ms": statistics.fmean(case["mean_latency_ms"] for case in cases),
        "mean_cpu_ms": statistics.fmean(case["mean_cpu_ms"] for case in cases),
        "process_rss_bytes": process.memory_info().rss,
        "child_peak_rss_kib": resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--fixtures",
        type=Path,
        default=Path("crates/vision/tests/fixtures/ocr"),
    )
    parser.add_argument("--iterations", type=int, default=3)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if not 1 <= args.iterations <= 100:
        raise SystemExit("--iterations must be within 1 through 100")
    rapid = RapidOCR()
    report = {
        "schema": 1,
        "iterations": args.iterations,
        "fixtures": str(args.fixtures),
        "notes": [
            "Tesseract is measured as a fresh CLI process per crop.",
            "RapidOCR uses ONNX Runtime and its full detection/classification/recognition pipeline.",
            "RSS measurements are high-water diagnostics, not isolated allocator totals.",
        ],
        "results": [
            benchmark("tesseract-5", tesseract_once, args.fixtures, args.iterations),
            benchmark(
                "rapidocr-onnxruntime",
                lambda path: rapid_once(rapid, path),
                args.fixtures,
                args.iterations,
            ),
        ],
    }
    rendered = json.dumps(report, indent=2, ensure_ascii=False) + "\n"
    if args.output:
        args.output.write_text(rendered, encoding="utf-8")
    else:
        print(rendered, end="")


if __name__ == "__main__":
    main()
