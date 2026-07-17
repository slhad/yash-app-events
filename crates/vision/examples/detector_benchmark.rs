use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

use yash_app_events_capture::{Frame, FrameLayout, PixelFormat};
use yash_app_events_profile::{BarDirection, NormalizedRegion};
use yash_app_events_vision::{
    ColorBarConfig, ColorBarDetector, Detector, GrayImage, PreprocessPipeline, RegionChangeConfig,
    RegionChangeDetector, Template, TemplateConfig, TemplateDetector,
};

const ITERATIONS: u32 = 10_000;

fn main() {
    let frame = fixture();
    let region = NormalizedRegion {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    };
    let color = ColorBarDetector::new(ColorBarConfig {
        direction: BarDirection::LeftToRight,
        minimum_rgb: [180, 0, 0],
        maximum_rgb: [255, 80, 80],
        line_match_fraction: 0.8,
        maximum_gap_fraction: 0.02,
        mask: None,
    })
    .unwrap();
    let template_image = GrayImage::new(5, 5, vec![76; 25]).unwrap();
    let template = TemplateDetector::new(TemplateConfig {
        templates: vec![Template {
            name: "red".into(),
            image: template_image,
            mask: None,
        }],
        threshold: 0.8,
        preprocessing: PreprocessPipeline::default(),
    })
    .unwrap();
    let change = RegionChangeDetector::new(RegionChangeConfig {
        change_threshold: 0.1,
        preprocessing: PreprocessPipeline::default(),
    })
    .unwrap();
    run("color_bar", color, &frame, region);
    run("template", template, &frame, region);
    run("region_change", change, &frame, region);
}

fn run(name: &str, mut detector: impl Detector, frame: &Frame, region: NormalizedRegion) {
    let start = Instant::now();
    for _ in 0..ITERATIONS {
        black_box(detector.detect(black_box(frame), region));
    }
    let elapsed = start.elapsed();
    println!(
        "{name}: {:.3} us/evaluation ({ITERATIONS} iterations)",
        elapsed.as_secs_f64() * 1_000_000.0 / f64::from(ITERATIONS)
    );
}

fn fixture() -> Frame {
    let width = 100_usize;
    let height = 20_usize;
    let mut bytes = vec![0_u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let offset = (y * width + x) * 4;
            bytes[offset..offset + 4].copy_from_slice(if x < 50 {
                &[220, 20, 20, 255]
            } else {
                &[10, 10, 10, 255]
            });
        }
    }
    Frame::new(
        0,
        Duration::ZERO,
        FrameLayout {
            width: u32::try_from(width).unwrap(),
            height: u32::try_from(height).unwrap(),
            row_stride: width * 4,
            format: PixelFormat::Rgba8,
        },
        Some("benchmark".into()),
        Arc::from(bytes),
    )
    .unwrap()
}
