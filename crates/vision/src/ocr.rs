use std::fmt;
use std::io::Cursor;
use std::path::PathBuf;

use leptess::{LepTess, Variable};
use serde::{Deserialize, Serialize};
use yash_app_events_capture::Frame;
use yash_app_events_profile::{NormalizedRegion, PreprocessOperation};

use crate::{
    grayscale_crop, Detection, DetectionStatus, DetectionValue, Detector, PreprocessPipeline,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OcrConfig {
    pub language: String,
    pub data_path: Option<PathBuf>,
    pub page_segmentation_mode: u8,
    pub character_whitelist: Option<String>,
    pub change_trigger_threshold: f32,
    pub maximum_interval_ms: u64,
    pub preprocessing: PreprocessPipeline,
    pub empty_value: Option<String>,
    pub zero_pad_to: Option<u8>,
}

pub struct OcrDetector {
    config: OcrConfig,
    engine: LepTess,
    previous: Option<crate::GrayImage>,
    last_detection: Option<Detection>,
    last_run_ms: Option<u64>,
}

impl fmt::Debug for OcrDetector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OcrDetector")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl OcrDetector {
    /// Initializes the native Tesseract backend and validates bounded configuration.
    ///
    /// # Errors
    ///
    /// Returns an actionable initialization or configuration error.
    pub fn new(config: OcrConfig) -> Result<Self, String> {
        if config.language.is_empty()
            || config.language.len() > 32
            || config.page_segmentation_mode > 13
            || !config.change_trigger_threshold.is_finite()
            || !(0.0..=1.0).contains(&config.change_trigger_threshold)
            || !(100..=60_000).contains(&config.maximum_interval_ms)
            || config
                .character_whitelist
                .as_ref()
                .is_some_and(|whitelist| whitelist.len() > 256)
            || config
                .zero_pad_to
                .is_some_and(|width| !(1..=16).contains(&width))
        {
            return Err("invalid OCR language, page segmentation mode, or whitelist".into());
        }
        let data_path = config.data_path.as_deref().and_then(|path| path.to_str());
        let mut engine = LepTess::new(data_path, &config.language)
            .map_err(|error| format!("failed to initialize Tesseract: {error}"))?;
        engine
            .set_variable(
                Variable::TesseditPagesegMode,
                &config.page_segmentation_mode.to_string(),
            )
            .map_err(|error| format!("failed to configure page segmentation: {error}"))?;
        if let Some(whitelist) = &config.character_whitelist {
            engine
                .set_variable(Variable::TesseditCharWhitelist, whitelist)
                .map_err(|error| format!("failed to configure character whitelist: {error}"))?;
        }
        Ok(Self {
            config,
            engine,
            previous: None,
            last_detection: None,
            last_run_ms: None,
        })
    }

    fn recognize_binary_fallback(&mut self, image: &crate::GrayImage) -> Option<String> {
        let binary = PreprocessPipeline {
            operations: vec![PreprocessOperation::Threshold {
                minimum: 77,
                maximum: 255,
            }],
        }
        .apply(image)
        .ok()?;
        let encoded = encode_grayscale_png(&binary).ok()?;
        self.engine.set_image_from_mem(&encoded).ok()?;
        let text = self.engine.get_utf8_text().ok()?.trim().to_owned();
        (!text.is_empty()).then_some(text)
    }
}

impl Detector for OcrDetector {
    fn detect(&mut self, frame: &Frame, region: NormalizedRegion) -> Detection {
        let image = match grayscale_crop(frame, region)
            .and_then(|image| self.config.preprocessing.apply(&image))
        {
            Ok(image) => image,
            Err(error) => return Detection::error(format!("OCR preprocessing failed: {error}")),
        };
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
        let encoded = match encode_grayscale_png(&image) {
            Ok(encoded) => encoded,
            Err(error) => return Detection::error(error),
        };
        if let Err(error) = self.engine.set_image_from_mem(&encoded) {
            return Detection::error(format!("Tesseract rejected the OCR crop: {error}"));
        }
        let mut text = match self.engine.get_utf8_text() {
            Ok(text) => text.trim().to_owned(),
            Err(error) => {
                return Detection::error(format!("Tesseract recognition failed: {error}"))
            }
        };
        let mut binary_fallback = false;
        if text.is_empty() {
            if let Some(fallback_text) = self.recognize_binary_fallback(&image) {
                text = fallback_text;
                binary_fallback = true;
            }
        }
        if text.is_empty() {
            self.last_run_ms = Some(timestamp_ms);
            if let Some(value) = &self.config.empty_value {
                let detection = Detection {
                    value: Some(DetectionValue::Text(value.clone())),
                    confidence: Some(1.0),
                    status: DetectionStatus::Valid,
                    diagnostic: format!("optional OCR value absent; emitted configured {value}"),
                };
                self.last_detection = Some(detection.clone());
                return detection;
            }
            if let Some(mut cached) = self.last_detection.clone() {
                cached.confidence = cached.confidence.map(|confidence| confidence * 0.8);
                cached
                    .diagnostic
                    .push_str("; retained after transient empty OCR result");
                return cached;
            }
            return Detection::unknown("Tesseract found no text");
        }
        let zero_padded = zero_pad_text(&mut text, self.config.zero_pad_to);
        #[allow(clippy::cast_precision_loss)]
        let confidence = (self.engine.mean_text_conf() as f32 / 100.0).clamp(0.0, 1.0);
        let detection = Detection {
            value: Some(DetectionValue::Text(text.clone())),
            confidence: Some(confidence),
            status: DetectionStatus::Valid,
            diagnostic: format!(
                "Tesseract recognized {} character(s){}{}",
                text.chars().count(),
                if binary_fallback {
                    " after binary fallback"
                } else {
                    ""
                },
                if zero_padded {
                    " with zero padding"
                } else {
                    ""
                }
            ),
        };
        self.last_run_ms = Some(timestamp_ms);
        self.last_detection = Some(detection.clone());
        detection
    }
}

fn zero_pad_text(text: &mut String, width: Option<u8>) -> bool {
    let Some(width) = width else { return false };
    let missing = usize::from(width).saturating_sub(text.chars().count());
    if missing == 0 {
        return false;
    }
    text.insert_str(0, &"0".repeat(missing));
    true
}

fn normalized_difference(previous: &crate::GrayImage, current: &crate::GrayImage) -> f32 {
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

fn encode_grayscale_png(image: &crate::GrayImage) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(
            Cursor::new(&mut bytes),
            u32::try_from(image.width).map_err(|_| "OCR crop width exceeds PNG limits")?,
            u32::try_from(image.height).map_err(|_| "OCR crop height exceeds PNG limits")?,
        );
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|error| format!("failed to encode OCR crop: {error}"))?;
        writer
            .write_image_data(&image.pixels)
            .map_err(|error| format!("failed to encode OCR pixels: {error}"))?;
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use yash_app_events_capture::{FrameLayout, PixelFormat};

    use super::*;

    #[test]
    fn numeric_width_is_preserved_without_truncating_longer_results() {
        let mut short = "5".to_owned();
        assert!(zero_pad_text(&mut short, Some(2)));
        assert_eq!(short, "05");
        let mut complete = "116".to_owned();
        assert!(!zero_pad_text(&mut complete, Some(2)));
        assert_eq!(complete, "116");
    }

    #[test]
    fn rejects_unbounded_configuration_before_native_initialization() {
        let error = OcrDetector::new(OcrConfig {
            language: "eng".into(),
            data_path: None,
            page_segmentation_mode: 14,
            character_whitelist: None,
            change_trigger_threshold: 0.02,
            maximum_interval_ms: 1_000,
            preprocessing: PreprocessPipeline::default(),
            empty_value: None,
            zero_pad_to: None,
        })
        .unwrap_err();
        assert!(error.contains("invalid OCR"));
    }

    #[test]
    fn recognizes_redistributable_synthetic_hud_text() {
        let frame = fixture_frame(include_bytes!("../tests/fixtures/ocr/victory.png"));
        let mut detector = OcrDetector::new(OcrConfig {
            language: "eng".into(),
            data_path: None,
            page_segmentation_mode: 7,
            character_whitelist: Some("ABCDEFGHIJKLMNOPQRSTUVWXYZ".into()),
            change_trigger_threshold: 0.02,
            maximum_interval_ms: 1_000,
            preprocessing: PreprocessPipeline::default(),
            empty_value: None,
            zero_pad_to: None,
        })
        .unwrap();
        let detection = detector.detect(
            &frame,
            NormalizedRegion {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
        );
        assert_eq!(detection.status, DetectionStatus::Valid);
        let Some(DetectionValue::Text(text)) = detection.value else {
            panic!("expected OCR text");
        };
        assert_eq!(text, "VICTORY");
        assert!(detection
            .confidence
            .is_some_and(|confidence| confidence > 0.5));
        let cached = detector.detect(
            &frame,
            NormalizedRegion {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
        );
        assert!(cached
            .diagnostic
            .contains("cached because crop is unchanged"));
    }

    #[test]
    fn optional_counter_emits_zero_when_text_is_absent() {
        let frame = Frame::new(
            0,
            Duration::ZERO,
            FrameLayout {
                width: 40,
                height: 40,
                row_stride: 120,
                format: PixelFormat::Rgb8,
            },
            Some("blank-optional-counter".into()),
            Arc::from(vec![0_u8; 40 * 40 * 3]),
        )
        .unwrap();
        let mut detector = OcrDetector::new(OcrConfig {
            language: "eng".into(),
            data_path: None,
            page_segmentation_mode: 10,
            character_whitelist: Some("0123456789".into()),
            change_trigger_threshold: 0.01,
            maximum_interval_ms: 500,
            preprocessing: PreprocessPipeline::default(),
            empty_value: Some("0".into()),
            zero_pad_to: None,
        })
        .unwrap();
        let detection = detector.detect(
            &frame,
            NormalizedRegion {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
        );
        assert_eq!(detection.status, DetectionStatus::Valid);
        assert_eq!(detection.value, Some(DetectionValue::Text("0".into())));
    }

    #[test]
    fn localization_scale_animation_and_glow_fixtures_have_a_recorded_baseline() {
        let fixtures: [(&str, &[u8], &str); 5] = [
            (
                "clean",
                include_bytes!("../tests/fixtures/ocr/victory.png"),
                "VICTORY",
            ),
            (
                "localized",
                include_bytes!("../tests/fixtures/ocr/localized.png"),
                "NIVEAU ETE",
            ),
            (
                "scaled",
                include_bytes!("../tests/fixtures/ocr/scaled.png"),
                "VICTORY",
            ),
            (
                "animated",
                include_bytes!("../tests/fixtures/ocr/animated.png"),
                "VICTORY",
            ),
            (
                "glow",
                include_bytes!("../tests/fixtures/ocr/glow.png"),
                "VICTORY",
            ),
        ];
        let mut regressions = Vec::new();
        for (name, bytes, expected) in fixtures {
            let mut detector = OcrDetector::new(OcrConfig {
                language: "eng".into(),
                data_path: None,
                page_segmentation_mode: 7,
                character_whitelist: None,
                change_trigger_threshold: 0.02,
                maximum_interval_ms: 1_000,
                preprocessing: PreprocessPipeline::default(),
                empty_value: None,
                zero_pad_to: None,
            })
            .unwrap();
            let detection = detector.detect(
                &fixture_frame(bytes),
                NormalizedRegion {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
            );
            let observed = match detection.value {
                Some(DetectionValue::Text(text)) => text,
                _ => String::new(),
            };
            regressions.push((name, observed, expected));
        }
        assert!(
            regressions
                .iter()
                .all(|(_, observed, expected)| observed == expected),
            "OCR fixture baseline regressed: {regressions:?}"
        );
    }

    fn fixture_frame(bytes: &[u8]) -> Frame {
        let decoder = png::Decoder::new(Cursor::new(bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut output = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut output).unwrap();
        assert_eq!(info.color_type, png::ColorType::Grayscale);
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
            Some("ocr-fixture".into()),
            Arc::from(rgb),
        )
        .unwrap()
    }
}
