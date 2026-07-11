use serde::{Deserialize, Serialize};
use yash_app_events_capture::Frame;
use yash_app_events_profile::NormalizedRegion;

use crate::{grayscale_crop, Detection, DetectionStatus, Detector, GrayImage, PreprocessPipeline};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RegionChangeConfig {
    pub change_threshold: f32,
    pub preprocessing: PreprocessPipeline,
}

/// Stateful normalized mean-absolute-difference detector.
#[derive(Clone, Debug)]
pub struct RegionChangeDetector {
    config: RegionChangeConfig,
    previous: Option<GrayImage>,
}

impl RegionChangeDetector {
    /// Creates a validated detector.
    ///
    /// # Errors
    ///
    /// Rejects thresholds outside zero through one.
    pub fn new(config: RegionChangeConfig) -> Result<Self, &'static str> {
        if !config.change_threshold.is_finite() || !(0.0..=1.0).contains(&config.change_threshold) {
            return Err("change threshold must be within [0,1]");
        }
        Ok(Self {
            config,
            previous: None,
        })
    }
}

impl Detector for RegionChangeDetector {
    #[allow(clippy::cast_precision_loss)]
    fn detect(&mut self, frame: &Frame, region: NormalizedRegion) -> Detection {
        let current = match grayscale_crop(frame, region)
            .and_then(|image| self.config.preprocessing.apply(&image))
        {
            Ok(image) => image,
            Err(error) => return Detection::error(error),
        };
        let Some(previous) = self.previous.replace(current.clone()) else {
            return Detection::unknown("baseline frame established");
        };
        if previous.width != current.width || previous.height != current.height {
            return Detection::unknown("baseline reset after crop dimensions changed");
        }
        let difference = previous
            .pixels
            .iter()
            .zip(&current.pixels)
            .map(|(previous, current)| u64::from(previous.abs_diff(*current)))
            .sum::<u64>() as f64
            / current.pixels.len() as f64
            / 255.0;
        Detection {
            value: Some(difference),
            confidence: Some(1.0),
            status: DetectionStatus::Valid,
            diagnostic: format!(
                "change {difference:.4}; stable={}",
                difference < f64::from(self.config.change_threshold)
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use yash_app_events_capture::{FrameLayout, PixelFormat};

    fn frame(value: u8) -> Frame {
        Frame::new(
            0,
            Duration::ZERO,
            FrameLayout {
                width: 2,
                height: 2,
                row_stride: 8,
                format: PixelFormat::Rgba8,
            },
            None,
            Arc::from([value, value, value, 255].repeat(4)),
        )
        .unwrap()
    }

    #[test]
    fn baseline_is_unknown_then_change_is_normalized() {
        let mut detector = RegionChangeDetector::new(RegionChangeConfig {
            change_threshold: 0.1,
            preprocessing: PreprocessPipeline::default(),
        })
        .unwrap();
        let region = NormalizedRegion {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        };
        assert_eq!(
            detector.detect(&frame(0), region).status,
            DetectionStatus::Unknown
        );
        let changed = detector.detect(&frame(255), region);
        assert_eq!(changed.status, DetectionStatus::Valid);
        assert_eq!(changed.value, Some(1.0));
    }
}
