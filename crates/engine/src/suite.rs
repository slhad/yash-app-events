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

/// Opt-in wall-clock timings for one regression-suite evaluation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SuiteTimingSummary {
    pub inventory_ms: u64,
    pub profile_load_ms: u64,
    pub total_cases_ms: u64,
    pub serialization_ms: u64,
    pub slowest_cases: Vec<SuiteCaseTiming>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SuiteCaseTiming {
    pub id: String,
    pub duration_ms: u64,
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
    #[serde(default)]
    pub provenance: Option<SuiteProvenance>,
    pub frames: Vec<SuiteFrame>,
    #[serde(default)]
    pub check_events: bool,
    #[serde(default)]
    pub expected_events: Vec<ExpectedEvent>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SuiteProvenance {
    pub source: String,
    #[serde(default)]
    pub source_session: Option<String>,
    #[serde(default)]
    pub captured_at: Option<String>,
    pub original_width: u32,
    pub original_height: u32,
    pub original_sha256: String,
    pub review_state: String,
    pub no_account_secret: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SuiteFrame {
    pub image: PathBuf,
    #[serde(default)]
    pub timestamp_ms: u64,
    pub placement: FramePlacement,
    #[serde(default)]
    pub expected_observations: BTreeMap<String, ExpectedObservation>,
    #[serde(default)]
    pub expected_frame: Option<ExpectedFrameIdentity>,
    #[serde(default)]
    pub expected_scene: Option<ExpectedScene>,
    /// Exact ordered set of active overlay names. Omission disables this assertion.
    #[serde(default)]
    pub expected_overlays: Option<Vec<String>>,
    #[serde(default)]
    pub expected_targets: BTreeMap<String, ExpectedTarget>,
    #[serde(default)]
    pub expected_ai_handoff: Option<ExpectedAiHandoff>,
}

impl SuiteFrame {
    #[must_use]
    pub fn has_spatial_assertions(&self) -> bool {
        self.expected_frame.is_some()
            || self.expected_scene.is_some()
            || self.expected_overlays.is_some()
            || !self.expected_targets.is_empty()
            || self.expected_ai_handoff.is_some()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExpectedFrameIdentity {
    pub width: u32,
    pub height: u32,
    pub sha256: String,
    #[serde(default = "default_true")]
    pub layout_compatible: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExpectedScene {
    pub status: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub minimum_confidence: Option<f32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExpectedTarget {
    pub visibility: String,
    #[serde(default)]
    pub scene: Option<String>,
    #[serde(default)]
    pub overlay: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub caution_class: Option<String>,
    #[serde(default)]
    pub rectangle: Option<PixelRectangle>,
    #[serde(default)]
    pub safe_point: Option<PixelPoint>,
    #[serde(default)]
    pub pixel_tolerance: u32,
    #[serde(default)]
    pub caution_contains: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PixelPoint {
    pub x: u32,
    pub y: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExpectedAiHandoff {
    pub json_sufficient: bool,
    pub visual_review_recommended: bool,
    #[serde(default)]
    pub reason: Option<String>,
    pub full_frame_recommended: bool,
    #[serde(default)]
    pub suggested_crop_names: Vec<String>,
}

const fn default_true() -> bool {
    true
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
                },
                "expected_frame": {
                    "width":558,"height":992,
                    "sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                },
                "expected_scene":{"status":"recognized","name":"home","minimum_confidence":0.9},
                "expected_overlays":[],
                "expected_targets":{
                    "quest_button":{
                        "visibility":"visible","scene":"home","role":"navigation",
                        "caution_class":"navigation",
                        "rectangle":{"x":100,"y":900,"width":80,"height":60},
                        "safe_point":{"x":140,"y":930}
                    }
                },
                "expected_ai_handoff":{
                    "json_sufficient":true,"visual_review_recommended":false,
                    "full_frame_recommended":false,"suggested_crop_names":[]
                }
            }]
        }))
        .expect("valid suite case");
        assert!(matches!(
            case.frames[0].placement,
            FramePlacement::ZoneCrop { .. }
        ));
        assert!(case.frames[0].has_spatial_assertions());
        assert_eq!(
            case.frames[0].expected_targets["quest_button"].caution_class,
            Some("navigation".into())
        );

        let placement: FramePlacement = serde_json::from_value(serde_json::json!({
            "type":"partial_frame",
            "source_region":{"x":0,"y":0,"width":530,"height":254}
        }))
        .expect("valid partial placement");
        assert!(matches!(placement, FramePlacement::PartialFrame { .. }));
    }
}
