use std::fmt;
use std::fs;
use std::path::PathBuf;

use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use sha2::{Digest as _, Sha256};
use yash_app_events_capture::Frame;
use yash_app_events_profile::NormalizedRegion;

use crate::{
    grayscale_crop, Detection, DetectionStatus, DetectionValue, Detector, GrayImage,
    PreprocessPipeline,
};

#[derive(Clone, Debug)]
pub struct ClassifierConfig {
    pub model_path: PathBuf,
    pub model_sha256: String,
    pub labels: Vec<String>,
    pub input_width: usize,
    pub input_height: usize,
    pub preprocessing: PreprocessPipeline,
    pub change_trigger_threshold: f32,
    pub maximum_interval_ms: u64,
}

pub struct OnnxClassifierDetector {
    config: ClassifierConfig,
    session: Session,
    previous: Option<GrayImage>,
    last_detection: Option<Detection>,
    last_run_ms: Option<u64>,
}

impl fmt::Debug for OnnxClassifierDetector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OnnxClassifierDetector")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl OnnxClassifierDetector {
    /// Validates and loads one bounded CPU ONNX classifier.
    ///
    /// # Errors
    ///
    /// Rejects invalid hashes, labels, dimensions, scheduling, files, and model graphs.
    pub fn new(config: ClassifierConfig) -> Result<Self, String> {
        if config.labels.len() < 2
            || config.labels.len() > 256
            || config
                .labels
                .iter()
                .any(|label| label.is_empty() || label.len() > 64)
            || config.input_width == 0
            || config.input_height == 0
            || config
                .input_width
                .checked_mul(config.input_height)
                .is_none_or(|pixels| pixels > 16_777_216)
            || !config.change_trigger_threshold.is_finite()
            || !(0.0..=1.0).contains(&config.change_trigger_threshold)
            || !(100..=60_000).contains(&config.maximum_interval_ms)
            || config.model_sha256.len() != 64
            || !config
                .model_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("invalid classifier labels, dimensions, scheduling, or hash".into());
        }
        let metadata = fs::metadata(&config.model_path)
            .map_err(|error| format!("cannot inspect classifier model: {error}"))?;
        if !metadata.is_file() || metadata.len() > 64 * 1024 * 1024 {
            return Err("classifier model must be a regular file no larger than 64 MiB".into());
        }
        let model = fs::read(&config.model_path)
            .map_err(|error| format!("cannot read classifier model: {error}"))?;
        let digest = format!("{:x}", Sha256::digest(&model));
        if !digest.eq_ignore_ascii_case(&config.model_sha256) {
            return Err("classifier model SHA-256 does not match the profile".into());
        }
        let session = Session::builder()
            .and_then(|builder| builder.with_optimization_level(GraphOptimizationLevel::Level1))
            .and_then(|builder| builder.with_intra_threads(1))
            .and_then(|builder| builder.commit_from_memory(&model))
            .map_err(|error| format!("failed to load ONNX classifier: {error}"))?;
        Ok(Self {
            config,
            session,
            previous: None,
            last_detection: None,
            last_run_ms: None,
        })
    }
}

impl Detector for OnnxClassifierDetector {
    fn detect(&mut self, frame: &Frame, region: NormalizedRegion) -> Detection {
        let image = match grayscale_crop(frame, region)
            .and_then(|image| self.config.preprocessing.apply(&image))
        {
            Ok(image) => image,
            Err(error) => {
                return Detection::error(format!("classifier preprocessing failed: {error}"))
            }
        };
        if image.width != self.config.input_width || image.height != self.config.input_height {
            return Detection::error(format!(
                "classifier preprocessing produced {}x{}, expected {}x{}",
                image.width, image.height, self.config.input_width, self.config.input_height
            ));
        }
        let timestamp_ms = u64::try_from(frame.timestamp.as_millis()).unwrap_or(u64::MAX);
        let changed = self.previous.as_ref().is_none_or(|previous| {
            normalized_difference(previous, &image) >= self.config.change_trigger_threshold
        });
        let refresh_due = self.last_run_ms.is_none_or(|last| {
            timestamp_ms.saturating_sub(last) >= self.config.maximum_interval_ms
        });
        self.previous = Some(image.clone());
        if !changed && !refresh_due {
            if let Some(mut cached) = self.last_detection.clone() {
                cached
                    .diagnostic
                    .push_str("; cached because crop is unchanged");
                return cached;
            }
        }
        let input: Vec<_> = image
            .pixels
            .iter()
            .map(|pixel| f32::from(*pixel) / 255.0)
            .collect();
        let tensor = match Tensor::from_array((
            [1, 1, self.config.input_height, self.config.input_width],
            input.into_boxed_slice(),
        )) {
            Ok(tensor) => tensor,
            Err(error) => return Detection::error(format!("classifier input failed: {error}")),
        };
        let outputs = match self.session.run(ort::inputs![tensor]) {
            Ok(outputs) => outputs,
            Err(error) => return Detection::error(format!("classifier inference failed: {error}")),
        };
        let (_, logits) = match outputs[0].try_extract_tensor::<f32>() {
            Ok(output) => output,
            Err(error) => return Detection::error(format!("classifier output failed: {error}")),
        };
        if logits.len() != self.config.labels.len() {
            return Detection::error("classifier output count does not match labels");
        }
        let Some((best_index, best_logit)) = logits
            .iter()
            .copied()
            .enumerate()
            .max_by(|(_, left), (_, right)| left.total_cmp(right))
        else {
            return Detection::unknown("classifier returned no logits");
        };
        let maximum = best_logit;
        let denominator: f32 = logits.iter().map(|logit| (*logit - maximum).exp()).sum();
        let confidence = if denominator > 0.0 {
            1.0 / denominator
        } else {
            0.0
        };
        let label = self.config.labels[best_index].clone();
        let detection = Detection {
            value: Some(DetectionValue::Text(label.clone())),
            confidence: Some(confidence),
            status: DetectionStatus::Valid,
            diagnostic: format!("ONNX classifier selected {label}"),
        };
        self.last_run_ms = Some(timestamp_ms);
        self.last_detection = Some(detection.clone());
        detection
    }
}

fn normalized_difference(previous: &GrayImage, current: &GrayImage) -> f32 {
    if previous.width != current.width || previous.height != current.height {
        return 1.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let difference = previous
        .pixels
        .iter()
        .zip(&current.pixels)
        .map(|(left, right)| u64::from(left.abs_diff(*right)))
        .sum::<u64>() as f32
        / (previous.pixels.len().max(1) as f32 * 255.0);
    difference
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::Arc;
    use std::time::Duration;

    use yash_app_events_capture::{FrameLayout, PixelFormat};
    use yash_app_events_profile::PreprocessOperation;

    use super::*;

    const MODEL: &str = "tests/fixtures/classifier/hud_icon.onnx";
    const HASH: &str = "12ac2f734bbe111526ef82db676086b75a30635a28c2ab8a032a1b1f10759fc6";

    fn detector(hash: &str) -> Result<OnnxClassifierDetector, String> {
        OnnxClassifierDetector::new(ClassifierConfig {
            model_path: MODEL.into(),
            model_sha256: hash.into(),
            labels: vec!["orb".into(), "cross".into()],
            input_width: 8,
            input_height: 8,
            preprocessing: PreprocessPipeline {
                operations: vec![PreprocessOperation::Resize {
                    width: 8,
                    height: 8,
                }],
            },
            change_trigger_threshold: 0.02,
            maximum_interval_ms: 1_000,
        })
    }

    #[test]
    fn rejects_model_integrity_mismatch_before_session_creation() {
        assert!(detector(&"0".repeat(64)).unwrap_err().contains("SHA-256"));
    }

    #[test]
    fn synthetic_dataset_classifies_all_cases_and_caches_unchanged_crop() {
        let cases: [(&[u8], &str); 8] = [
            (
                include_bytes!("../tests/fixtures/classifier/orb_0.png"),
                "orb",
            ),
            (
                include_bytes!("../tests/fixtures/classifier/orb_1.png"),
                "orb",
            ),
            (
                include_bytes!("../tests/fixtures/classifier/orb_2.png"),
                "orb",
            ),
            (
                include_bytes!("../tests/fixtures/classifier/orb_3.png"),
                "orb",
            ),
            (
                include_bytes!("../tests/fixtures/classifier/cross_0.png"),
                "cross",
            ),
            (
                include_bytes!("../tests/fixtures/classifier/cross_1.png"),
                "cross",
            ),
            (
                include_bytes!("../tests/fixtures/classifier/cross_2.png"),
                "cross",
            ),
            (
                include_bytes!("../tests/fixtures/classifier/cross_3.png"),
                "cross",
            ),
        ];
        for (bytes, expected) in cases {
            let mut detector = detector(HASH).unwrap();
            let frame = fixture_frame(bytes);
            let detection = detector.detect(&frame, full_region());
            assert_eq!(detection.value, Some(DetectionValue::Text(expected.into())));
            assert!(detection
                .confidence
                .is_some_and(|confidence| confidence > 0.5));
            let cached = detector.detect(&frame, full_region());
            assert!(cached
                .diagnostic
                .contains("cached because crop is unchanged"));
        }
    }

    fn full_region() -> NormalizedRegion {
        NormalizedRegion {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        }
    }

    fn fixture_frame(bytes: &[u8]) -> Frame {
        let decoder = png::Decoder::new(Cursor::new(bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut output = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut output).unwrap();
        let rgb: Vec<_> = output[..info.buffer_size()]
            .iter()
            .flat_map(|value| [*value, *value, *value])
            .collect();
        Frame::new(
            0,
            Duration::ZERO,
            FrameLayout {
                width: info.width,
                height: info.height,
                row_stride: usize::try_from(info.width).unwrap() * 3,
                format: PixelFormat::Rgb8,
            },
            Some("classifier-fixture".into()),
            Arc::from(rgb),
        )
        .unwrap()
    }
}
