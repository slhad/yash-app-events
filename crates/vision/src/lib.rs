//! Deterministic detector implementations and explicit image preprocessing boundaries.

use yash_app_events_capture::{Frame, PixelFormat};
use yash_app_events_profile::{BarDirection, NormalizedRegion};

mod preprocess;
mod region_change;
mod template;
pub use preprocess::{GrayImage, PreprocessPipeline};
pub use region_change::{RegionChangeConfig, RegionChangeDetector};
pub use template::{Template, TemplateConfig, TemplateDetector};
pub use yash_app_events_profile::PreprocessOperation;

/// Detector output before the engine attaches stable detector/element identities.
#[derive(Clone, Debug, serde::Deserialize, PartialEq, serde::Serialize)]
pub struct Detection {
    pub value: Option<f64>,
    pub confidence: Option<f32>,
    pub status: DetectionStatus,
    pub diagnostic: String,
}

impl Detection {
    #[must_use]
    pub fn unknown(diagnostic: impl Into<String>) -> Self {
        Self {
            value: None,
            confidence: None,
            status: DetectionStatus::Unknown,
            diagnostic: diagnostic.into(),
        }
    }

    #[must_use]
    pub fn error(diagnostic: impl Into<String>) -> Self {
        Self {
            value: None,
            confidence: None,
            status: DetectionStatus::Error,
            diagnostic: diagnostic.into(),
        }
    }
}

/// Detector failures never imply a negative observation.
#[derive(Clone, Copy, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionStatus {
    Valid,
    Unknown,
    Error,
}

/// Project-owned detector boundary shared by live and replay capture.
pub trait Detector: std::fmt::Debug + Send {
    fn detect(&mut self, frame: &Frame, region: NormalizedRegion) -> Detection;
}

/// Runtime configuration for deterministic color/range bar measurement.
#[derive(Clone, Debug)]
pub struct ColorBarConfig {
    pub direction: BarDirection,
    pub minimum_rgb: [u8; 3],
    pub maximum_rgb: [u8; 3],
    /// Required matching fraction across each line perpendicular to fill direction.
    pub line_match_fraction: f32,
    /// Optional row-major binary mask matching the detector crop dimensions.
    pub mask: Option<Vec<bool>>,
}

/// Measures the contiguous matching fill from one configured edge.
#[derive(Clone, Debug)]
pub struct ColorBarDetector {
    config: ColorBarConfig,
}

impl ColorBarDetector {
    /// Creates a validated color bar detector.
    ///
    /// # Errors
    ///
    /// Rejects inverted RGB ranges and fractions outside zero through one.
    pub fn new(config: ColorBarConfig) -> Result<Self, &'static str> {
        if config
            .minimum_rgb
            .iter()
            .zip(config.maximum_rgb)
            .any(|(minimum, maximum)| *minimum > maximum)
        {
            return Err("minimum RGB channels must not exceed maximum channels");
        }
        if !config.line_match_fraction.is_finite()
            || !(0.0..=1.0).contains(&config.line_match_fraction)
        {
            return Err("line match fraction must be within [0,1]");
        }
        Ok(Self { config })
    }
}

impl Detector for ColorBarDetector {
    #[allow(clippy::cast_precision_loss)]
    fn detect(&mut self, frame: &Frame, region: NormalizedRegion) -> Detection {
        let Some(crop) = Crop::new(frame, region) else {
            return Detection::error("invalid or empty crop");
        };
        if self
            .config
            .mask
            .as_ref()
            .is_some_and(|mask| mask.len() != crop.width * crop.height)
        {
            return Detection::error("mask dimensions do not match crop");
        }
        let horizontal = matches!(
            self.config.direction,
            BarDirection::LeftToRight | BarDirection::RightToLeft
        );
        let lines = if horizontal { crop.width } else { crop.height };
        let mut matching_lines = Vec::with_capacity(lines);
        let mut line_scores = Vec::with_capacity(lines);
        for line in 0..lines {
            let samples = if horizontal { crop.height } else { crop.width };
            let mut matched = 0_usize;
            let mut included = 0_usize;
            for sample in 0..samples {
                let (x, y) = if horizontal {
                    (line, sample)
                } else {
                    (sample, line)
                };
                if self
                    .config
                    .mask
                    .as_ref()
                    .is_some_and(|mask| !mask[y * crop.width + x])
                {
                    continue;
                }
                included += 1;
                matched += usize::from(crop.rgb(x, y).is_some_and(|rgb| {
                    in_range(rgb, self.config.minimum_rgb, self.config.maximum_rgb)
                }));
            }
            let score = if included == 0 {
                0.0
            } else {
                matched as f32 / included as f32
            };
            line_scores.push(score);
            matching_lines.push(score >= self.config.line_match_fraction);
        }
        let reverse = matches!(
            self.config.direction,
            BarDirection::RightToLeft | BarDirection::BottomToTop
        );
        let contiguous = if reverse {
            matching_lines
                .iter()
                .rev()
                .take_while(|&&matched| matched)
                .count()
        } else {
            matching_lines
                .iter()
                .take_while(|&&matched| matched)
                .count()
        };
        let fill = contiguous as f64 / lines as f64;
        let confidence = if lines == 0 {
            0.0
        } else {
            line_scores.iter().sum::<f32>() / lines as f32
        };
        Detection {
            value: Some(fill),
            confidence: Some(confidence),
            status: DetectionStatus::Valid,
            diagnostic: format!("matched {contiguous}/{lines} fill lines"),
        }
    }
}

fn in_range(rgb: [u8; 3], minimum: [u8; 3], maximum: [u8; 3]) -> bool {
    rgb.into_iter()
        .zip(minimum)
        .zip(maximum)
        .all(|((value, minimum), maximum)| (minimum..=maximum).contains(&value))
}

struct Crop<'a> {
    frame: &'a Frame,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

/// Converts a validated frame region into a tightly packed grayscale crop.
///
/// # Errors
///
/// Rejects invalid/empty regions and truncated frame storage.
pub fn grayscale_crop(frame: &Frame, region: NormalizedRegion) -> Result<GrayImage, &'static str> {
    let crop = Crop::new(frame, region).ok_or("invalid or empty crop")?;
    let mut pixels = Vec::with_capacity(crop.width * crop.height);
    for y in 0..crop.height {
        for x in 0..crop.width {
            let [red, green, blue] = crop.rgb(x, y).ok_or("crop pixel exceeds frame buffer")?;
            let luminance =
                (u32::from(red) * 77 + u32::from(green) * 150 + u32::from(blue) * 29) >> 8;
            pixels.push(u8::try_from(luminance).unwrap_or(u8::MAX));
        }
    }
    GrayImage::new(crop.width, crop.height, pixels)
}

impl<'a> Crop<'a> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn new(frame: &'a Frame, region: NormalizedRegion) -> Option<Self> {
        if [region.x, region.y, region.width, region.height]
            .iter()
            .any(|value| !value.is_finite())
            || region.x < 0.0
            || region.y < 0.0
            || region.width <= 0.0
            || region.height <= 0.0
            || region.x + region.width > 1.0
            || region.y + region.height > 1.0
        {
            return None;
        }
        let x = (f64::from(region.x) * f64::from(frame.width)).floor() as usize;
        let y = (f64::from(region.y) * f64::from(frame.height)).floor() as usize;
        let right = (f64::from(region.x + region.width) * f64::from(frame.width))
            .ceil()
            .min(f64::from(frame.width)) as usize;
        let bottom = (f64::from(region.y + region.height) * f64::from(frame.height))
            .ceil()
            .min(f64::from(frame.height)) as usize;
        let width = right.checked_sub(x)?;
        let height = bottom.checked_sub(y)?;
        (width > 0 && height > 0).then_some(Self {
            frame,
            x,
            y,
            width,
            height,
        })
    }

    fn rgb(&self, x: usize, y: usize) -> Option<[u8; 3]> {
        let bytes = self.frame.format.bytes_per_pixel();
        let offset = (self.y + y)
            .checked_mul(self.frame.row_stride)?
            .checked_add((self.x + x).checked_mul(bytes)?)?;
        let pixel = self.frame.data.get(offset..offset + bytes)?;
        match self.frame.format {
            PixelFormat::Rgb8 | PixelFormat::Rgba8 => Some([pixel[0], pixel[1], pixel[2]]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use yash_app_events_capture::FrameLayout;

    fn bar_frame(fill_columns: usize, padded: bool) -> Frame {
        let width = 10_usize;
        let height = 2_usize;
        let stride = if padded { width * 4 + 8 } else { width * 4 };
        let mut bytes = vec![0_u8; stride * height];
        for y in 0..height {
            for x in 0..width {
                let offset = y * stride + x * 4;
                bytes[offset..offset + 4].copy_from_slice(if x < fill_columns {
                    &[220, 20, 20, 255]
                } else {
                    &[20, 20, 20, 255]
                });
            }
        }
        Frame::new(
            0,
            Duration::ZERO,
            FrameLayout {
                width: u32::try_from(width).unwrap(),
                height: u32::try_from(height).unwrap(),
                row_stride: stride,
                format: PixelFormat::Rgba8,
            },
            None,
            Arc::from(bytes),
        )
        .unwrap()
    }

    fn detector(direction: BarDirection) -> ColorBarDetector {
        ColorBarDetector::new(ColorBarConfig {
            direction,
            minimum_rgb: [180, 0, 0],
            maximum_rgb: [255, 60, 60],
            line_match_fraction: 0.75,
            mask: None,
        })
        .unwrap()
    }

    #[test]
    fn measures_left_to_right_fill_with_padded_stride() {
        let result = detector(BarDirection::LeftToRight).detect(
            &bar_frame(4, true),
            NormalizedRegion {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
        );
        assert_eq!(result.status, DetectionStatus::Valid);
        assert!((result.value.unwrap() - 0.4).abs() < f64::EPSILON);
    }

    #[test]
    fn wrong_edge_reports_zero_not_discontinuous_matches() {
        let result = detector(BarDirection::RightToLeft).detect(
            &bar_frame(4, false),
            NormalizedRegion {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
        );
        assert_eq!(result.value, Some(0.0));
    }

    #[test]
    fn mismatched_mask_is_an_error_without_negative_value() {
        let mut detector = detector(BarDirection::LeftToRight);
        detector.config.mask = Some(vec![true]);
        let result = detector.detect(
            &bar_frame(4, false),
            NormalizedRegion {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
        );
        assert_eq!(result.status, DetectionStatus::Error);
        assert_eq!(result.value, None);
    }

    #[test]
    fn scaled_bar_tolerates_minor_noise_and_brightness_variation() {
        let width = 20_usize;
        let height = 4_usize;
        let mut bytes = vec![0_u8; width * height * 4];
        for y in 0..height {
            for x in 0..width {
                let offset = (y * width + x) * 4;
                let pixel = if x < 10 && !(x == 4 && y == 0) {
                    if (x + y) % 2 == 0 {
                        [190, 10, 10, 255]
                    } else {
                        [245, 55, 55, 255]
                    }
                } else {
                    [30, 30, 30, 255]
                };
                bytes[offset..offset + 4].copy_from_slice(&pixel);
            }
        }
        let frame = Frame::new(
            0,
            Duration::ZERO,
            FrameLayout {
                width: 20,
                height: 4,
                row_stride: 80,
                format: PixelFormat::Rgba8,
            },
            None,
            Arc::from(bytes),
        )
        .unwrap();
        let result = detector(BarDirection::LeftToRight).detect(
            &frame,
            NormalizedRegion {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
        );
        assert_eq!(result.status, DetectionStatus::Valid);
        assert_eq!(result.value, Some(0.5));
    }
}
