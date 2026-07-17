//! Durable contracts for passively collected, reviewable detector evidence.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::suite::ExpectedObservation;
use crate::{Observation, Transition};

/// One screenshot and the detector evidence produced for its exact analyzed frame.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CollectionItem {
    pub schema: u32,
    pub id: String,
    pub created_at: String,
    pub game: String,
    pub profile_id: String,
    pub profile_revision: u64,
    pub profile_sha256: String,
    pub frame: CollectedFrame,
    pub image_sha256: String,
    pub observations: BTreeMap<String, Observation>,
    #[serde(default)]
    pub transitions: Vec<Transition>,
    pub reason: CollectionReason,
    pub review: ReviewRecord,
    /// A bounded 32x18 grayscale signature used only for similarity decisions.
    pub thumbnail: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CollectedFrame {
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub width: u32,
    pub height: u32,
    pub pixel_format: String,
    pub source: Option<String>,
    pub image: PathBuf,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CollectionReason {
    pub trigger: String,
    pub scene_difference: Option<f32>,
    pub scene_novel: bool,
    pub evidence_novel: bool,
    #[serde(default)]
    pub evidence_reasons: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Pending,
    Accepted,
    Rejected,
    Promoted,
    NeedsCorrection,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ReviewRecord {
    pub status: ReviewStatus,
    pub reviewer: Option<String>,
    pub reviewed_at: Option<String>,
    pub reason: Option<String>,
    #[serde(default)]
    pub expected_observations: BTreeMap<String, ExpectedObservation>,
    pub promoted_case: Option<PathBuf>,
}

impl Default for ReviewRecord {
    fn default() -> Self {
        Self {
            status: ReviewStatus::Pending,
            reviewer: None,
            reviewed_at: None,
            reason: None,
            expected_observations: BTreeMap::new(),
            promoted_case: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_review_never_fabricates_expected_values() {
        let review = ReviewRecord::default();
        assert_eq!(review.status, ReviewStatus::Pending);
        assert!(review.expected_observations.is_empty());
        assert!(review.promoted_case.is_none());
    }
}
