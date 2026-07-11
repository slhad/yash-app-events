use serde::{Deserialize, Serialize};
use yash_app_events_capture::Frame;
use yash_app_events_profile::NormalizedRegion;

use crate::{grayscale_crop, Detection, DetectionStatus, Detector, GrayImage, PreprocessPipeline};

/// One named grayscale template and optional row-major mask.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Template {
    pub name: String,
    pub image: GrayImage,
    pub mask: Option<Vec<bool>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TemplateConfig {
    pub templates: Vec<Template>,
    pub threshold: f32,
    pub preprocessing: PreprocessPipeline,
}

/// Sliding normalized template matcher with best-match diagnostics.
#[derive(Clone, Debug)]
pub struct TemplateDetector {
    config: TemplateConfig,
}

impl TemplateDetector {
    /// Creates a validated matcher.
    ///
    /// # Errors
    ///
    /// Rejects empty template sets, invalid thresholds, masks, and names.
    pub fn new(config: TemplateConfig) -> Result<Self, &'static str> {
        if config.templates.is_empty() {
            return Err("at least one template is required");
        }
        if !config.threshold.is_finite() || !(0.0..=1.0).contains(&config.threshold) {
            return Err("template threshold must be within [0,1]");
        }
        for template in &config.templates {
            if template.name.is_empty() {
                return Err("template name must not be empty");
            }
            if template
                .mask
                .as_ref()
                .is_some_and(|mask| mask.len() != template.image.pixels.len())
            {
                return Err("template mask dimensions do not match");
            }
        }
        Ok(Self { config })
    }
}

impl Detector for TemplateDetector {
    fn detect(&mut self, frame: &Frame, region: NormalizedRegion) -> Detection {
        let crop = match grayscale_crop(frame, region)
            .and_then(|image| self.config.preprocessing.apply(&image))
        {
            Ok(crop) => crop,
            Err(error) => return Detection::error(error),
        };
        let mut best: Option<(&str, f32, usize, usize)> = None;
        for template in &self.config.templates {
            if template.image.width > crop.width || template.image.height > crop.height {
                continue;
            }
            for y in 0..=crop.height - template.image.height {
                for x in 0..=crop.width - template.image.width {
                    let score = normalized_score(&crop, x, y, template);
                    if best.is_none_or(|(_, best_score, _, _)| score > best_score) {
                        best = Some((&template.name, score, x, y));
                    }
                }
            }
        }
        let Some((name, score, x, y)) = best else {
            return Detection::unknown("all templates exceed processed crop dimensions");
        };
        Detection {
            value: Some(f64::from(score >= self.config.threshold)),
            confidence: Some(score),
            status: DetectionStatus::Valid,
            diagnostic: format!("best template {name} at {x},{y} score {score:.4}"),
        }
    }
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn normalized_score(
    crop: &GrayImage,
    origin_x: usize,
    origin_y: usize,
    template: &Template,
) -> f32 {
    let mut source = Vec::new();
    let mut target = Vec::new();
    for y in 0..template.image.height {
        for x in 0..template.image.width {
            let index = y * template.image.width + x;
            if template.mask.as_ref().is_some_and(|mask| !mask[index]) {
                continue;
            }
            source.push(f64::from(crop.pixel(origin_x + x, origin_y + y)));
            target.push(f64::from(template.image.pixels[index]));
        }
    }
    if source.is_empty() {
        return 0.0;
    }
    let count = source.len() as f64;
    let source_mean = source.iter().sum::<f64>() / count;
    let target_mean = target.iter().sum::<f64>() / count;
    let mut numerator = 0.0;
    let mut source_variance = 0.0;
    let mut target_variance = 0.0;
    for (source, target) in source.iter().zip(&target) {
        let source_delta = source - source_mean;
        let target_delta = target - target_mean;
        numerator += source_delta * target_delta;
        source_variance += source_delta * source_delta;
        target_variance += target_delta * target_delta;
    }
    if source_variance <= f64::EPSILON || target_variance <= f64::EPSILON {
        let error = source
            .iter()
            .zip(&target)
            .map(|(source, target)| (source - target).abs())
            .sum::<f64>()
            / count
            / 255.0;
        (1.0 - error).clamp(0.0, 1.0) as f32
    } else {
        ((numerator / (source_variance * target_variance).sqrt() + 1.0) * 0.5).clamp(0.0, 1.0)
            as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use yash_app_events_capture::{FrameLayout, PixelFormat};

    #[test]
    fn selects_best_masked_template_under_brightness_shift() {
        let mut pixels = vec![20_u8; 5 * 5 * 4];
        for &(x, y) in &[(2, 1), (1, 2), (2, 2), (3, 2), (2, 3)] {
            let offset = (y * 5 + x) * 4;
            pixels[offset..offset + 4].copy_from_slice(&[180, 180, 180, 255]);
        }
        let frame = Frame::new(
            0,
            Duration::ZERO,
            FrameLayout {
                width: 5,
                height: 5,
                row_stride: 20,
                format: PixelFormat::Rgba8,
            },
            None,
            Arc::from(pixels),
        )
        .unwrap();
        let cross = GrayImage::new(3, 3, vec![0, 150, 0, 150, 150, 150, 0, 150, 0]).unwrap();
        let square = GrayImage::new(3, 3, vec![150; 9]).unwrap();
        let mut detector = TemplateDetector::new(TemplateConfig {
            templates: vec![
                Template {
                    name: "square".into(),
                    image: square,
                    mask: None,
                },
                Template {
                    name: "cross".into(),
                    image: cross,
                    mask: Some(vec![true; 9]),
                },
            ],
            threshold: 0.9,
            preprocessing: PreprocessPipeline::default(),
        })
        .unwrap();
        let result = detector.detect(
            &frame,
            NormalizedRegion {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
        );
        assert_eq!(result.value, Some(1.0));
        assert!(result.diagnostic.contains("cross"));
    }
}
