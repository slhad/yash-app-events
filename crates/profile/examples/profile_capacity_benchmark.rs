use std::time::Instant;

use yash_app_events_profile::{
    BarDirection, Detector, DetectorId, Element, ElementId, NormalizedRegion, Profile,
};

fn main() {
    for size in [128_usize, 256, 384, 512, 768, 1024] {
        let profile = generated_profile(size);
        let serialize_started = Instant::now();
        let encoded = serde_json::to_vec(&profile).expect("generated profile serializes");
        let serialize_us = serialize_started.elapsed().as_micros();
        let parse_started = Instant::now();
        let parsed: Profile = serde_json::from_slice(&encoded).expect("generated profile parses");
        let parse_us = parse_started.elapsed().as_micros();
        let validation_started = Instant::now();
        let valid = parsed.validate().is_ok();
        let validation_us = validation_started.elapsed().as_micros();
        let report = parsed.analyze_capacity();
        println!(
            "{}",
            serde_json::json!({
                "elements":size,
                "bytes":encoded.len(),
                "serialize_us":serialize_us,
                "parse_us":parse_us,
                "validation_us":validation_us,
                "valid_under_current_limit":valid,
                "detector_families":report.detector_families,
            })
        );
    }
}

fn generated_profile(size: usize) -> Profile {
    let mut profile = Profile::new("Generated capacity workload", "benchmark", 1920, 1080);
    for index in 0..size {
        let detector = match index % 4 {
            0 => Detector::ColorBar {
                id: DetectorId::new(),
                direction: BarDirection::LeftToRight,
                minimum_rgb: [0, 0, 0],
                maximum_rgb: [255, 255, 255],
                mask: None,
            },
            1 => Detector::Template {
                id: DetectorId::new(),
                templates: vec!["templates/shared.json".into()],
                masks: Vec::new(),
                threshold: 0.9,
                preprocessing: Vec::new(),
            },
            2 => Detector::Ocr {
                id: DetectorId::new(),
                language: "eng".into(),
                page_segmentation_mode: 7,
                character_whitelist: None,
                change_trigger_threshold: 0.01,
                maximum_interval_ms: 1_000,
                preprocessing: Vec::new(),
                empty_value: None,
                zero_pad_to: None,
                retry: None,
            },
            _ => Detector::RegionChange {
                id: DetectorId::new(),
                threshold: 0.05,
                preprocessing: Vec::new(),
            },
        };
        profile.elements.push(Element {
            id: ElementId::new(),
            name: format!("element-{index}"),
            enabled: true,
            color: "#ffffff".into(),
            region: NormalizedRegion {
                x: 0.0,
                y: 0.0,
                width: 0.01,
                height: 0.01,
            },
            detector,
        });
    }
    profile
}
