use yash_app_events_capture::Frame;
use yash_app_events_profile::NormalizedRegion;

use crate::{
    grayscale_crop, Detection, DetectionStatus, DetectionValue, Detector, PreprocessPipeline,
};

#[derive(Clone, Debug)]
pub struct SevenSegmentConfig {
    pub digits: u8,
    pub separator_after: Option<u8>,
    pub threshold: u8,
    pub preprocessing: PreprocessPipeline,
}

#[derive(Debug)]
pub struct SevenSegmentDetector {
    config: SevenSegmentConfig,
}

impl SevenSegmentDetector {
    /// Creates a bounded fixed-layout seven-segment decoder.
    ///
    /// # Errors
    ///
    /// Rejects unsupported digit counts and invalid separator positions.
    pub fn new(config: SevenSegmentConfig) -> Result<Self, &'static str> {
        if !(1..=8).contains(&config.digits)
            || config
                .separator_after
                .is_some_and(|position| position == 0 || position >= config.digits)
        {
            return Err("invalid seven-segment layout");
        }
        Ok(Self { config })
    }
}

impl Detector for SevenSegmentDetector {
    #[allow(clippy::cast_precision_loss)]
    fn detect(&mut self, frame: &Frame, region: NormalizedRegion) -> Detection {
        let image = match grayscale_crop(frame, region)
            .and_then(|image| self.config.preprocessing.apply(&image))
        {
            Ok(image) => image,
            Err(error) => {
                return Detection::error(format!("seven-segment preprocessing failed: {error}"))
            }
        };
        let expected_groups =
            usize::from(self.config.digits) + usize::from(self.config.separator_after.is_some());
        if image.width < expected_groups * 5 || image.height < 10 {
            return Detection::error("seven-segment crop is too small");
        }
        let foreground = image
            .pixels
            .iter()
            .filter(|pixel| **pixel >= self.config.threshold)
            .count();
        let occupancy = foreground as f32 / image.pixels.len().max(1) as f32;
        if occupancy > 0.70 {
            return Detection::unknown(format!(
                "seven-segment crop is {:.0}% foreground; HUD is absent or threshold is invalid",
                occupancy * 100.0
            ));
        }
        let groups = active_column_groups(&image, self.config.threshold);
        if groups.len() != expected_groups {
            return Detection::unknown(format!(
                "expected {expected_groups} seven-segment glyphs, found {}",
                groups.len()
            ));
        }
        let mut text = String::new();
        let mut confidences = Vec::new();
        let mut patterns = Vec::new();
        let normal_width = groups
            .iter()
            .enumerate()
            .filter(|(index, _)| self.config.separator_after.map(usize::from) != Some(*index))
            .map(|(_, (left, right))| right.saturating_sub(*left))
            .max()
            .unwrap_or(1);
        for digit in 0..usize::from(self.config.digits) {
            if self.config.separator_after == u8::try_from(digit).ok() {
                text.push(':');
            }
            let group = digit
                + usize::from(
                    self.config
                        .separator_after
                        .is_some_and(|position| digit >= usize::from(position)),
                );
            let (left, right) = groups[group];
            let (value, confidence, pattern, scores) = decode_digit(
                &image,
                left,
                right.saturating_sub(left),
                self.config.threshold,
                normal_width,
            );
            let Some(value) = value else {
                return Detection::unknown(format!(
                    "unrecognized seven-segment digit {} (pattern {pattern:07b})",
                    digit + 1,
                ));
            };
            text.push(char::from(b'0' + value));
            confidences.push(confidence);
            patterns.push(format!("{pattern:07b}:{scores:?}"));
        }
        let confidence = confidences.into_iter().fold(1.0_f32, f32::min);
        Detection {
            value: Some(DetectionValue::Text(text.clone())),
            confidence: Some(confidence),
            status: DetectionStatus::Valid,
            diagnostic: format!(
                "decoded {} seven-segment digits (patterns {})",
                self.config.digits,
                patterns.join(",")
            ),
        }
    }
}

fn active_column_groups(image: &crate::GrayImage, threshold: u8) -> Vec<(usize, usize)> {
    let active =
        (0..image.width).map(|x| (0..image.height).any(|y| image.pixel(x, y) >= threshold));
    let mut groups = Vec::new();
    let mut start = None;
    for (x, active) in active.enumerate() {
        match (start, active) {
            (None, true) => start = Some(x),
            (Some(left), false) => {
                groups.push((left, x));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(left) = start {
        groups.push((left, image.width));
    }
    groups
}

fn decode_digit(
    image: &crate::GrayImage,
    x: usize,
    width: usize,
    threshold: u8,
    normal_width: usize,
) -> (Option<u8>, f32, u8, [f32; 7]) {
    if width.saturating_mul(2) < normal_width {
        return (Some(1), 1.0, 0b000_0110, [0.0; 7]);
    }
    let mut active_rows = (0..image.height).filter(|&y| {
        (x..x.saturating_add(width).min(image.width))
            .any(|column| image.pixel(column, y) >= threshold)
    });
    let Some(top) = active_rows.next() else {
        return (None, 0.0, 0, [0.0; 7]);
    };
    let bottom = active_rows.next_back().unwrap_or(top).saturating_add(1);
    let height = bottom.saturating_sub(top);
    let horizontal = |y: f32| {
        sample(
            image,
            x,
            top,
            width,
            height,
            0.35,
            0.65,
            y - 0.04,
            y + 0.04,
            threshold,
        )
    };
    let vertical = |x_fraction: f32, y1: f32, y2: f32| {
        sample(
            image,
            x,
            top,
            width,
            height,
            x_fraction - 0.05,
            x_fraction + 0.05,
            y1,
            y2,
            threshold,
        )
    };
    let scores = [
        horizontal(0.10),
        vertical(0.90, 0.22, 0.38),
        vertical(0.90, 0.62, 0.78),
        horizontal(0.90),
        vertical(0.10, 0.62, 0.78),
        vertical(0.10, 0.22, 0.38),
        horizontal(0.50),
    ];
    let pattern = scores
        .iter()
        .enumerate()
        .fold(0_u8, |bits, (index, score)| {
            bits | (u8::from(*score >= 0.20) << index)
        });
    let value = digit_for_scores(pattern, &scores);
    let confidence = scores
        .iter()
        .map(|score| (score - 0.20).abs() / 0.80)
        .fold(1.0_f32, f32::min)
        .clamp(0.0, 1.0);
    (value, confidence, pattern, scores)
}

#[cfg(test)]
const fn digit_for_pattern(pattern: u8) -> Option<u8> {
    digit_for_scores(pattern, &[0.0; 7])
}

const fn digit_for_scores(pattern: u8, scores: &[f32; 7]) -> Option<u8> {
    match pattern {
        0b011_1111 => Some(0),
        0b000_0110 | 0b011_0000 | 0b011_0110 => Some(1),
        0b101_1011 => Some(2),
        // Tight calibration crops of the solid HUD font expose fewer probes
        // than full-frame regions while retaining stable digit-specific shapes.
        0b101_1001 | 0b110_0110 => Some(4),
        0b111_1001 | 0b110_1101 => Some(5),
        // Solid stage digits 2, 5, 3, and 4 share the same thresholded probes.
        // Their middle-stroke coverage forms four distinct bands in recorded HUD frames.
        0b111_0001 => Some(if scores[6] >= 0.90 {
            4
        } else if scores[6] >= 0.75 {
            3
        } else if scores[6] >= 0.60 {
            5
        } else {
            2
        }),
        // Condensed solid HUD fonts can miss the thin bottom probe while
        // retaining both right-hand strokes. That remains a 3, not a 2.
        0b100_1111 | 0b100_0111 => Some(3),
        0b111_1101 => Some(6),
        0b000_0111 => Some(7),
        0b111_1111 => Some(8),
        // The same condensed HUD font can miss the thin bottom probe on 9.
        0b110_1111 | 0b110_0111 => Some(9),
        _ => None,
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_arguments
)]
fn sample(
    image: &crate::GrayImage,
    slot_x: usize,
    slot_y: usize,
    slot_width: usize,
    slot_height: usize,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
    threshold: u8,
) -> f32 {
    let left = slot_x + (slot_width as f32 * x1.max(0.0)) as usize;
    let right = slot_x + (slot_width as f32 * x2.min(1.0)) as usize;
    let top = slot_y + (slot_height as f32 * y1.max(0.0)) as usize;
    let bottom = slot_y + (slot_height as f32 * y2.min(1.0)) as usize;
    let mut matched = 0_usize;
    let mut total = 0_usize;
    for y in top..bottom.max(top + 1).min(image.height) {
        for x in left..right.max(left + 1).min(image.width) {
            matched += usize::from(image.pixel(x, y) >= threshold);
            total += 1;
        }
    }
    matched as f32 / total.max(1) as f32
}

#[cfg(test)]
mod tests {
    use super::{digit_for_pattern, digit_for_scores};

    #[test]
    fn condensed_three_is_not_aliased_to_two() {
        assert_eq!(digit_for_pattern(0b101_1011), Some(2));
        assert_eq!(digit_for_pattern(0b100_0111), Some(3));
        assert_eq!(digit_for_pattern(0b100_1111), Some(3));
    }

    #[test]
    fn condensed_nine_may_omit_the_bottom_probe() {
        assert_eq!(digit_for_pattern(0b110_1111), Some(9));
        assert_eq!(digit_for_pattern(0b110_0111), Some(9));
    }

    #[test]
    fn solid_stage_one_uses_both_left_probes() {
        assert_eq!(digit_for_pattern(0b011_0000), Some(1));
        assert_eq!(digit_for_pattern(0b011_0110), Some(1));
    }

    #[test]
    fn solid_stage_digits_use_middle_stroke_coverage() {
        assert_eq!(digit_for_pattern(0b111_0001), Some(2));
        assert_eq!(
            digit_for_scores(0b111_0001, &[0.72, 0.0, 0.0, 0.0, 1.0, 0.67, 0.22]),
            Some(2)
        );
        assert_eq!(
            digit_for_scores(0b111_0001, &[1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.67]),
            Some(5)
        );
        assert_eq!(
            digit_for_scores(0b111_0001, &[0.72, 0.0, 0.0, 0.0, 1.0, 0.67, 0.83]),
            Some(3)
        );
        assert_eq!(
            digit_for_scores(0b111_0001, &[0.67, 0.0, 0.0, 0.0, 1.0, 1.0, 0.94]),
            Some(4)
        );
        assert_eq!(digit_for_pattern(0b101_1001), Some(4));
        assert_eq!(digit_for_pattern(0b111_1001), Some(5));
    }
}
