use serde::{Deserialize, Serialize};
use yash_app_events_capture::Frame;
use yash_app_events_profile::NormalizedRegion;

use crate::{
    grayscale_crop, Detection, DetectionStatus, DetectionValue, Detector, GrayImage,
    PreprocessPipeline,
};

const MAXIMUM_TEMPLATE_PIXEL_COMPARISONS: usize = 2_000_000;

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
    statistics: Vec<TemplateStatistics>,
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
            if template.image.width == 0
                || template.image.height == 0
                || template.image.width.checked_mul(template.image.height)
                    != Some(template.image.pixels.len())
            {
                return Err("template image dimensions do not match pixels");
            }
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
        let statistics = config
            .templates
            .iter()
            .map(TemplateStatistics::new)
            .collect();
        Ok(Self { config, statistics })
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
        let mut comparisons = 0_usize;
        for (template, statistics) in self.config.templates.iter().zip(&self.statistics) {
            if template.image.width > crop.width || template.image.height > crop.height {
                continue;
            }
            let template_comparisons = (crop.width - template.image.width + 1)
                .checked_mul(crop.height - template.image.height + 1)
                .and_then(|origins| origins.checked_mul(template.image.pixels.len()))
                .unwrap_or(usize::MAX);
            comparisons = comparisons.saturating_add(template_comparisons);
            if comparisons > MAXIMUM_TEMPLATE_PIXEL_COMPARISONS {
                return Detection::error("template search exceeds bounded comparison budget");
            }
            for y in 0..=crop.height - template.image.height {
                for x in 0..=crop.width - template.image.width {
                    let score = normalized_score(&crop, x, y, template, statistics);
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
            value: Some(DetectionValue::Number(f64::from(
                score >= self.config.threshold,
            ))),
            confidence: Some(score),
            status: DetectionStatus::Valid,
            diagnostic: format!("best template {name} at {x},{y} score {score:.4}"),
        }
    }
}

#[derive(Clone, Debug)]
struct TemplateStatistics {
    count: usize,
    mean: f64,
    variance: f64,
}

impl TemplateStatistics {
    #[allow(clippy::cast_precision_loss)]
    fn new(template: &Template) -> Self {
        let included = || {
            template
                .image
                .pixels
                .iter()
                .enumerate()
                .filter(|(index, _)| template.mask.as_ref().is_none_or(|mask| mask[*index]))
                .map(|(_, pixel)| f64::from(*pixel))
        };
        let count = included().count();
        let mean = included().sum::<f64>() / count.max(1) as f64;
        let variance = included().map(|pixel| (pixel - mean).powi(2)).sum();
        Self {
            count,
            mean,
            variance,
        }
    }
}

fn visit_template_pixels(
    crop: &GrayImage,
    origin_x: usize,
    origin_y: usize,
    template: &Template,
    mut visit: impl FnMut(f64, f64),
) {
    for y in 0..template.image.height {
        for x in 0..template.image.width {
            let index = y * template.image.width + x;
            if template.mask.as_ref().is_none_or(|mask| mask[index]) {
                visit(
                    f64::from(crop.pixel(origin_x + x, origin_y + y)),
                    f64::from(template.image.pixels[index]),
                );
            }
        }
    }
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn normalized_score(
    crop: &GrayImage,
    origin_x: usize,
    origin_y: usize,
    template: &Template,
    statistics: &TemplateStatistics,
) -> f32 {
    if statistics.count == 0 {
        return 0.0;
    }
    let count = statistics.count as f64;
    let mut error = 0.0;
    if statistics.variance <= f64::EPSILON {
        visit_template_pixels(crop, origin_x, origin_y, template, |source, target| {
            error += (source - target).abs();
        });
        return (1.0 - error / count / 255.0).clamp(0.0, 1.0) as f32;
    }
    let mut source_sum = 0.0;
    visit_template_pixels(crop, origin_x, origin_y, template, |source, _| {
        source_sum += source;
    });
    let source_mean = source_sum / count;
    let mut numerator = 0.0;
    let mut source_variance = 0.0;
    visit_template_pixels(crop, origin_x, origin_y, template, |source, target| {
        let source_delta = source - source_mean;
        numerator += source_delta * (target - statistics.mean);
        source_variance += source_delta * source_delta;
        error += (source - target).abs();
    });
    if source_variance <= f64::EPSILON {
        (1.0 - error / count / 255.0).clamp(0.0, 1.0) as f32
    } else {
        ((numerator / (source_variance * statistics.variance).sqrt() + 1.0) * 0.5).clamp(0.0, 1.0)
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
    fn optimized_scores_match_reference_for_masks_constants_and_search_positions() {
        let mut seed = 19_u32;
        let mut pixels = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            seed.to_le_bytes()[3]
        };
        for sample in 0..64 {
            let crop = GrayImage::new(
                12,
                11,
                (0..132)
                    .map(|_| if sample % 4 == 0 { 87 } else { pixels() })
                    .collect(),
            )
            .unwrap();
            let template = Template {
                name: "reference".into(),
                image: GrayImage::new(
                    5,
                    4,
                    (0..20)
                        .map(|_| if sample % 5 == 0 { 43 } else { pixels() })
                        .collect(),
                )
                .unwrap(),
                mask: match sample % 4 {
                    0 => None,
                    1 => Some(vec![false; 20]),
                    2 => Some((0..20).map(|index| index == 7).collect()),
                    _ => Some((0..20).map(|index| index % 3 != 0).collect()),
                },
            };
            let statistics = TemplateStatistics::new(&template);
            for y in 0..=crop.height - template.image.height {
                for x in 0..=crop.width - template.image.width {
                    assert_eq!(
                        normalized_score(&crop, x, y, &template, &statistics).to_bits(),
                        reference_score(&crop, x, y, &template).to_bits(),
                        "sample {sample}, position {x},{y}"
                    );
                }
            }
        }
    }

    #[test]
    fn rejects_malformed_deserialized_template_dimensions() {
        let result = TemplateDetector::new(TemplateConfig {
            templates: vec![Template {
                name: "malformed".into(),
                image: GrayImage {
                    width: 2,
                    height: 2,
                    pixels: vec![0],
                },
                mask: None,
            }],
            threshold: 0.8,
            preprocessing: PreprocessPipeline::default(),
        });
        assert!(result.is_err());
    }

    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    fn reference_score(
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
        assert_eq!(result.value, Some(DetectionValue::Number(1.0)));
        assert!(result.diagnostic.contains("cross"));
    }

    #[test]
    fn rejects_unbounded_sliding_search_before_scoring() {
        let frame = Frame::new(
            0,
            Duration::ZERO,
            FrameLayout {
                width: 1_500,
                height: 1_500,
                row_stride: 1_500 * 4,
                format: PixelFormat::Rgba8,
            },
            None,
            Arc::from(vec![0_u8; 1_500 * 1_500 * 4]),
        )
        .unwrap();
        let mut detector = TemplateDetector::new(TemplateConfig {
            templates: vec![Template {
                name: "pixel".into(),
                image: GrayImage::new(1, 1, vec![0]).unwrap(),
                mask: None,
            }],
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
        assert_eq!(result.status, DetectionStatus::Error);
        assert!(result.diagnostic.contains("bounded comparison budget"));
    }
}
