use serde::{Deserialize, Serialize};
use yash_app_events_profile::PreprocessOperation;

/// Project-owned deterministic grayscale image used for previews and detectors.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GrayImage {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

impl GrayImage {
    /// Constructs a tightly packed grayscale image.
    ///
    /// # Errors
    ///
    /// Rejects zero dimensions, overflow, and mismatched pixel storage.
    pub fn new(width: usize, height: usize, pixels: Vec<u8>) -> Result<Self, &'static str> {
        if width == 0 || height == 0 {
            return Err("image dimensions must be non-zero");
        }
        if width.checked_mul(height) != Some(pixels.len()) {
            return Err("grayscale pixel count does not match dimensions");
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    #[must_use]
    pub fn pixel(&self, x: usize, y: usize) -> u8 {
        self.pixels[y * self.width + x]
    }
}

/// Ordered deterministic operations usable by runtime and GUI preview.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreprocessPipeline {
    pub operations: Vec<PreprocessOperation>,
}

impl PreprocessPipeline {
    /// Applies every operation and returns the exact previewable detector input.
    ///
    /// # Errors
    ///
    /// Rejects zero/oversized resize dimensions and invalid thresholds.
    pub fn apply(&self, input: &GrayImage) -> Result<GrayImage, &'static str> {
        let mut image = input.clone();
        for operation in &self.operations {
            image = match *operation {
                PreprocessOperation::Resize { width, height } => resize(&image, width, height)?,
                PreprocessOperation::Threshold { minimum, maximum } => {
                    if minimum > maximum {
                        return Err("threshold minimum exceeds maximum");
                    }
                    GrayImage::new(
                        image.width,
                        image.height,
                        image
                            .pixels
                            .iter()
                            .map(|&pixel| {
                                if (minimum..=maximum).contains(&pixel) {
                                    255
                                } else {
                                    0
                                }
                            })
                            .collect(),
                    )?
                }
                PreprocessOperation::Erode { radius } => morphology(&image, radius, true)?,
                PreprocessOperation::Dilate { radius } => morphology(&image, radius, false)?,
                PreprocessOperation::Invert => GrayImage::new(
                    image.width,
                    image.height,
                    image.pixels.iter().map(|pixel| 255 - pixel).collect(),
                )?,
            };
        }
        Ok(image)
    }
}

fn resize(input: &GrayImage, width: usize, height: usize) -> Result<GrayImage, &'static str> {
    if width == 0
        || height == 0
        || width
            .checked_mul(height)
            .is_none_or(|pixels| pixels > 16_777_216)
    {
        return Err("resize dimensions are invalid or excessive");
    }
    let mut output = vec![0_u8; width * height];
    for y in 0..height {
        for x in 0..width {
            let source_x = x * input.width / width;
            let source_y = y * input.height / height;
            output[y * width + x] = input.pixel(source_x, source_y);
        }
    }
    GrayImage::new(width, height, output)
}

fn morphology(input: &GrayImage, radius: u8, erode: bool) -> Result<GrayImage, &'static str> {
    if radius > 8 {
        return Err("morphology radius exceeds 8");
    }
    let radius = usize::from(radius);
    let mut output = vec![0_u8; input.pixels.len()];
    for y in 0..input.height {
        for x in 0..input.width {
            let mut value = if erode { u8::MAX } else { u8::MIN };
            for sample_y in y.saturating_sub(radius)..=(y + radius).min(input.height - 1) {
                for sample_x in x.saturating_sub(radius)..=(x + radius).min(input.width - 1) {
                    value = if erode {
                        value.min(input.pixel(sample_x, sample_y))
                    } else {
                        value.max(input.pixel(sample_x, sample_y))
                    };
                }
            }
            output[y * input.width + x] = value;
        }
    }
    GrayImage::new(input.width, input.height, output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_is_serializable_previewable_and_deterministic() {
        let input = GrayImage::new(2, 2, vec![0, 100, 200, 255]).unwrap();
        let pipeline = PreprocessPipeline {
            operations: vec![
                PreprocessOperation::Resize {
                    width: 4,
                    height: 2,
                },
                PreprocessOperation::Threshold {
                    minimum: 90,
                    maximum: 210,
                },
                PreprocessOperation::Dilate { radius: 1 },
            ],
        };
        let first = pipeline.apply(&input).unwrap();
        let serialized = serde_json::to_string(&pipeline).unwrap();
        let restored: PreprocessPipeline = serde_json::from_str(&serialized).unwrap();
        assert_eq!(first, restored.apply(&input).unwrap());
        assert_eq!((first.width, first.height), (4, 2));
    }
}
