use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use yash_app_events_capture::{Frame, FrameLayout, PixelFormat};
use yash_app_events_profile::{NormalizedRegion, PreprocessOperation};
use yash_app_events_vision::{
    ClassifierConfig, DetectionValue, Detector as _, OnnxClassifierDetector, PreprocessPipeline,
};

const HASH: &str = "12ac2f734bbe111526ef82db676086b75a30635a28c2ab8a032a1b1f10759fc6";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/classifier");
    let cases = [
        ("orb_0.png", "orb"),
        ("orb_1.png", "orb"),
        ("orb_2.png", "orb"),
        ("orb_3.png", "orb"),
        ("cross_0.png", "cross"),
        ("cross_1.png", "cross"),
        ("cross_2.png", "cross"),
        ("cross_3.png", "cross"),
    ];
    let frames: Vec<_> = cases
        .iter()
        .enumerate()
        .map(|(index, (name, _))| decode_frame(&root.join(name), index as u64))
        .collect::<Result<_, _>>()?;
    let mut detector = OnnxClassifierDetector::new(ClassifierConfig {
        model_path: root.join("hud_icon.onnx"),
        model_sha256: HASH.into(),
        labels: vec!["orb".into(), "cross".into()],
        input_width: 8,
        input_height: 8,
        preprocessing: PreprocessPipeline {
            operations: vec![PreprocessOperation::Resize {
                width: 8,
                height: 8,
            }],
        },
        change_trigger_threshold: 0.0,
        maximum_interval_ms: 100,
    })?;
    let iterations = 10_000_u64;
    let ticks_before = cpu_ticks();
    let started = Instant::now();
    let mut correct = 0_u64;
    let mut confidence_sum = 0.0_f64;
    for _ in 0..iterations {
        for (frame, (_, expected)) in frames.iter().zip(&cases) {
            let detection = detector.detect(frame, full_region());
            if detection.value == Some(DetectionValue::Text((*expected).into())) {
                correct += 1;
            }
            confidence_sum += f64::from(detection.confidence.unwrap_or(0.0));
        }
    }
    let elapsed = started.elapsed();
    let evaluations = iterations * cases.len() as u64;
    let evaluations_f64 = f64::from(u32::try_from(evaluations)?);
    let correct_f64 = f64::from(u32::try_from(correct)?);
    let report = json!({
        "schema":1,
        "model":"hud_icon.onnx",
        "model_sha256":HASH,
        "iterations":iterations,
        "evaluations":evaluations,
        "accuracy":correct_f64 / evaluations_f64,
        "mean_confidence":confidence_sum / evaluations_f64,
        "mean_latency_ms":elapsed.as_secs_f64() * 1000.0 / evaluations_f64,
        "cpu_ticks":cpu_ticks().saturating_sub(ticks_before),
        "peak_rss_kib":peak_rss_kib(),
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn full_region() -> NormalizedRegion {
    NormalizedRegion {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    }
}

fn decode_frame(path: &std::path::Path, sequence: u64) -> Result<Frame, String> {
    let decoder = png::Decoder::new(Cursor::new(
        fs::read(path).map_err(|error| error.to_string())?,
    ));
    let mut reader = decoder.read_info().map_err(|error| error.to_string())?;
    let mut output = vec![0; reader.output_buffer_size().ok_or("PNG too large")?];
    let info = reader
        .next_frame(&mut output)
        .map_err(|error| error.to_string())?;
    let rgb: Vec<_> = output[..info.buffer_size()]
        .iter()
        .flat_map(|value| [*value, *value, *value])
        .collect();
    Frame::new(
        sequence,
        Duration::from_millis(sequence.saturating_mul(100)),
        FrameLayout {
            width: info.width,
            height: info.height,
            row_stride: usize::try_from(info.width).map_err(|error| error.to_string())? * 3,
            format: PixelFormat::Rgb8,
        },
        Some("classifier-benchmark".into()),
        Arc::from(rgb),
    )
    .map_err(|error| error.to_string())
}

fn cpu_ticks() -> u64 {
    fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|stat| {
            let after_name = stat.rsplit_once(") ")?.1;
            let fields: Vec<_> = after_name.split_whitespace().collect();
            Some(fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?)
        })
        .unwrap_or(0)
}

fn peak_rss_kib() -> u64 {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("VmHWM:")?
                    .split_whitespace()
                    .next()?
                    .parse()
                    .ok()
            })
        })
        .unwrap_or(0)
}
