//! Portable external detector-regression suite contracts.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{ExpectedEvent, ObservationStatus};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RegressionSuite {
    pub schema: u32,
    pub id: String,
    pub name: String,
    pub game: String,
    pub profile: PathBuf,
    pub cases: Vec<PathBuf>,
    pub files: Vec<SuiteFile>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SuiteFile {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RegressionCase {
    pub schema: u32,
    pub id: String,
    pub purpose: String,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub source_media: Option<PathBuf>,
    pub frames: Vec<SuiteFrame>,
    #[serde(default)]
    pub check_events: bool,
    #[serde(default)]
    pub expected_events: Vec<ExpectedEvent>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SuiteFrame {
    pub image: PathBuf,
    #[serde(default)]
    pub timestamp_ms: u64,
    pub placement: FramePlacement,
    #[serde(default)]
    pub expected_observations: BTreeMap<String, ExpectedObservation>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FramePlacement {
    FullFrame,
    PartialFrame { source_region: PixelRectangle },
    ZoneCrop { target: String },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PixelRectangle {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExpectedObservation {
    pub status: ObservationStatus,
    #[serde(default)]
    pub value: Option<ExpectedValue>,
    #[serde(default)]
    pub numeric_tolerance: Option<f64>,
    #[serde(default)]
    pub minimum_confidence: Option<f32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ExpectedValue {
    Boolean(bool),
    Number(f64),
    Text(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_and_zone_crop_cases_are_stable_external_json() {
        let case: RegressionCase = serde_json::from_value(serde_json::json!({
            "schema": 1,
            "id": "stage-5-purple",
            "purpose": "calibrate stage five",
            "categories": ["stage", "partial_frame"],
            "frames": [{
                "image": "media/stage-5.png",
                "placement": {"type":"zone_crop", "target":"stage_group"},
                "expected_observations": {
                    "stage_group": {"status":"valid", "value":"5", "minimum_confidence":0.1}
                }
            }]
        }))
        .expect("valid suite case");
        assert!(matches!(
            case.frames[0].placement,
            FramePlacement::ZoneCrop { .. }
        ));

        let placement: FramePlacement = serde_json::from_value(serde_json::json!({
            "type":"partial_frame",
            "source_region":{"x":0,"y":0,"width":530,"height":254}
        }))
        .expect("valid partial placement");
        assert!(matches!(placement, FramePlacement::PartialFrame { .. }));
    }
}
