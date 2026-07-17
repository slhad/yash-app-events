use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest as _, Sha256};
use yash_app_events_capture::Frame;
use yash_app_events_engine::collection::{CollectionItem, ReviewStatus};
use yash_app_events_engine::suite::{
    ExpectedObservation, ExpectedValue, FramePlacement, RegressionCase, RegressionSuite, SuiteFile,
    SuiteFrame,
};
use yash_app_events_engine::{Observation, ObservationStatus, ObservationValue};
use yash_app_events_profile::{atomic_write, CollectionPolicy, Profile};

const THUMBNAIL_WIDTH: usize = 32;
const THUMBNAIL_HEIGHT: usize = 18;

#[derive(Debug, Default)]
pub(crate) struct Runtime {
    pub policy: Option<CollectionPolicy>,
    pub profile_id: Option<String>,
    pub last_attempt: Option<Instant>,
    pub last_thumbnail: Option<Vec<u8>>,
    pub last_evidence: Option<String>,
    pub saved: u64,
    pub duplicates: u64,
    pub errors: u64,
    pub last_item: Option<String>,
    pub last_error: Option<String>,
    pub write_in_progress: bool,
}

impl Runtime {
    pub fn configure(&mut self, profile_id: String, policy: CollectionPolicy) {
        if self.profile_id.as_deref() != Some(profile_id.as_str()) {
            *self = Self::default();
            self.profile_id = Some(profile_id);
        }
        self.policy = Some(policy);
    }

    pub fn due(&self, now: Instant, frame_sequence: u64) -> bool {
        let Some(policy) = &self.policy else {
            return false;
        };
        if !policy.enabled || self.write_in_progress {
            return false;
        }
        let jitter_span = policy.jitter_seconds.saturating_mul(2).saturating_add(1);
        let jitter = if jitter_span == 0 {
            0_i64
        } else {
            i64::try_from(frame_sequence % jitter_span).unwrap_or(0)
                - i64::try_from(policy.jitter_seconds).unwrap_or(0)
        };
        let base = i64::try_from(policy.interval_seconds).unwrap_or(i64::MAX);
        let seconds = u64::try_from((base + jitter).max(10)).unwrap_or(10);
        self.last_attempt
            .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(seconds))
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct StorageStats {
    pub pending_items: usize,
    pub needs_correction_items: usize,
    pub accepted_items: usize,
    pub rejected_items: usize,
    pub promoted_items: usize,
    pub bytes: u64,
}

pub(crate) fn storage_stats(root: &Path) -> StorageStats {
    let mut stats = StorageStats {
        pending_items: 0,
        needs_correction_items: 0,
        accepted_items: 0,
        rejected_items: 0,
        promoted_items: 0,
        bytes: 0,
    };
    if root.as_os_str().is_empty() {
        return stats;
    }
    for (directory, status) in [
        ("inbox", ReviewStatus::Pending),
        ("accepted", ReviewStatus::Accepted),
        ("rejected", ReviewStatus::Rejected),
        ("promoted", ReviewStatus::Promoted),
    ] {
        let path = root.join(directory);
        let Ok(entries) = fs::read_dir(path) else {
            continue;
        };
        for entry in entries.flatten().filter(|entry| entry.path().is_dir()) {
            match status {
                ReviewStatus::Pending => {
                    let needs_correction = fs::read(entry.path().join("metadata.json"))
                        .ok()
                        .and_then(|bytes| serde_json::from_slice::<CollectionItem>(&bytes).ok())
                        .is_some_and(|item| item.review.status == ReviewStatus::NeedsCorrection);
                    if needs_correction {
                        stats.needs_correction_items += 1;
                    } else {
                        stats.pending_items += 1;
                    }
                }
                ReviewStatus::Accepted => stats.accepted_items += 1,
                ReviewStatus::Rejected => stats.rejected_items += 1,
                ReviewStatus::Promoted => stats.promoted_items += 1,
                ReviewStatus::NeedsCorrection => {}
            }
            for file in fs::read_dir(entry.path()).into_iter().flatten().flatten() {
                stats.bytes = stats
                    .bytes
                    .saturating_add(file.metadata().map_or(0, |metadata| metadata.len()));
            }
        }
    }
    stats
}

pub(crate) fn thumbnail(frame: &Frame) -> Vec<u8> {
    let mut result = vec![0_u8; THUMBNAIL_WIDTH * THUMBNAIL_HEIGHT];
    let width = usize::try_from(frame.width).unwrap_or(0);
    let height = usize::try_from(frame.height).unwrap_or(0);
    let bytes_per_pixel = frame.format.bytes_per_pixel();
    for cell_y in 0..THUMBNAIL_HEIGHT {
        let top = cell_y * height / THUMBNAIL_HEIGHT;
        let bottom = ((cell_y + 1) * height / THUMBNAIL_HEIGHT).max(top + 1);
        for cell_x in 0..THUMBNAIL_WIDTH {
            let left = cell_x * width / THUMBNAIL_WIDTH;
            let right = ((cell_x + 1) * width / THUMBNAIL_WIDTH).max(left + 1);
            let mut total = 0_u64;
            let mut count = 0_u64;
            for y in top..bottom.min(height) {
                for x in left..right.min(width) {
                    let offset = y * frame.row_stride + x * bytes_per_pixel;
                    let red = u64::from(frame.data[offset]);
                    let green = u64::from(frame.data[offset + 1]);
                    let blue = u64::from(frame.data[offset + 2]);
                    total += (red * 54 + green * 183 + blue * 19) / 256;
                    count += 1;
                }
            }
            result[cell_y * THUMBNAIL_WIDTH + cell_x] =
                u8::try_from(total / count.max(1)).unwrap_or(u8::MAX);
        }
    }
    result
}

#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub(crate) fn similarity(left: &[u8], right: &[u8]) -> Option<f32> {
    if left.len() != right.len() || left.is_empty() {
        return None;
    }
    let squared: f64 = left
        .iter()
        .zip(right)
        .map(|(left, right)| {
            let difference = f64::from(*left) - f64::from(*right);
            difference * difference
        })
        .sum();
    #[allow(clippy::cast_precision_loss)]
    let normalized = (squared / left.len() as f64).sqrt() / 255.0;
    Some(normalized as f32)
}

pub(crate) fn evidence_signature(
    observations: &BTreeMap<String, Observation>,
    novelty_targets: &[String],
) -> String {
    let signature: BTreeMap<_, _> = observations
        .iter()
        .map(|(name, observation)| {
            let include_value = novelty_targets.iter().any(|target| target == name);
            let confidence_band = observation.confidence.map(|confidence| {
                if confidence < 0.5 {
                    "low"
                } else if confidence < 0.8 {
                    "medium"
                } else {
                    "high"
                }
            });
            (
                name,
                serde_json::json!({
                    "status":observation.status,
                    "confidence":confidence_band,
                    "value":include_value.then_some(&observation.value)
                }),
            )
        })
        .collect();
    serde_json::to_string(&signature).unwrap_or_default()
}

pub(crate) fn persist_item(
    root: &Path,
    item: &CollectionItem,
    png: &[u8],
) -> std::io::Result<PathBuf> {
    let inbox = root.join("inbox");
    fs::create_dir_all(&inbox)?;
    let temporary = inbox.join(format!(".tmp-{}", item.id));
    let destination = inbox.join(&item.id);
    if temporary.exists() {
        fs::remove_dir_all(&temporary)?;
    }
    fs::create_dir(&temporary)?;
    atomic_write(&temporary.join("frame.png"), png)?;
    let metadata = serde_json::to_vec_pretty(item).map_err(std::io::Error::other)?;
    atomic_write(&temporary.join("metadata.json"), &metadata)?;
    fs::rename(&temporary, &destination)?;
    Ok(destination)
}

pub(crate) fn image_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn profile_sha256<T: Serialize>(profile: &T) -> String {
    serde_json::to_vec(profile)
        .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
        .unwrap_or_default()
}

pub(crate) fn list_items(root: &Path, requested: Option<ReviewStatus>) -> Vec<CollectionItem> {
    let mut items = Vec::new();
    if root.as_os_str().is_empty() {
        return items;
    }
    for (directory, status) in [
        ("inbox", ReviewStatus::Pending),
        ("accepted", ReviewStatus::Accepted),
        ("rejected", ReviewStatus::Rejected),
        ("promoted", ReviewStatus::Promoted),
    ] {
        if requested.is_some_and(|requested| {
            requested != status
                && !(requested == ReviewStatus::NeedsCorrection && directory == "inbox")
        }) {
            continue;
        }
        let Ok(entries) = fs::read_dir(root.join(directory)) else {
            continue;
        };
        for entry in entries.flatten().filter(|entry| entry.path().is_dir()) {
            let metadata = entry.path().join("metadata.json");
            if let Ok(bytes) = fs::read(metadata) {
                if let Ok(item) = serde_json::from_slice::<CollectionItem>(&bytes) {
                    if requested.is_some_and(|requested| requested != item.review.status) {
                        continue;
                    }
                    items.push(item);
                }
            }
        }
    }
    items.sort_by(|left: &CollectionItem, right: &CollectionItem| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.id.cmp(&right.id))
    });
    items
}

pub(crate) fn load_item(root: &Path, id: &str) -> std::io::Result<(PathBuf, CollectionItem)> {
    validate_item_id(id)?;
    for directory in ["inbox", "accepted", "rejected", "promoted"] {
        let item_root = root.join(directory).join(id);
        if item_root.is_dir() {
            let item = serde_json::from_slice(&fs::read(item_root.join("metadata.json"))?)
                .map_err(std::io::Error::other)?;
            return Ok((item_root, item));
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "collection item not found",
    ))
}

pub(crate) fn save_reviewed_item(
    root: &Path,
    current_root: &Path,
    item: &CollectionItem,
) -> std::io::Result<PathBuf> {
    let directory = match item.review.status {
        ReviewStatus::Pending | ReviewStatus::NeedsCorrection => "inbox",
        ReviewStatus::Accepted => "accepted",
        ReviewStatus::Rejected => "rejected",
        ReviewStatus::Promoted => "promoted",
    };
    atomic_write(
        &current_root.join("metadata.json"),
        &serde_json::to_vec_pretty(item).map_err(std::io::Error::other)?,
    )?;
    let destination = root.join(directory).join(&item.id);
    if destination != current_root {
        fs::create_dir_all(root.join(directory))?;
        fs::rename(current_root, &destination)?;
    }
    Ok(destination)
}

pub(crate) fn expected_from_observation(observation: &Observation) -> Option<ExpectedObservation> {
    if observation.status != ObservationStatus::Valid || observation.confidence.unwrap_or(0.0) < 0.8
    {
        return None;
    }
    let value = match &observation.value {
        ObservationValue::Boolean(value) => Some(ExpectedValue::Boolean(*value)),
        ObservationValue::Number(value) => Some(ExpectedValue::Number(*value)),
        ObservationValue::Text(value) => Some(ExpectedValue::Text(value.clone())),
        ObservationValue::None => None,
    };
    Some(ExpectedObservation {
        status: ObservationStatus::Valid,
        value,
        numeric_tolerance: None,
        minimum_confidence: Some(0.8),
    })
}

pub(crate) fn promote_item(
    root: &Path,
    current_root: &Path,
    item: &mut CollectionItem,
) -> std::io::Result<PathBuf> {
    let suite_path = root.join("suite.json");
    let mut suite: RegressionSuite =
        serde_json::from_slice(&fs::read(&suite_path)?).map_err(std::io::Error::other)?;
    if suite.schema != 1 || suite.game != item.game {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "collection item does not match suite schema/game",
        ));
    }
    let profile: Profile = serde_json::from_slice(&fs::read(root.join(&suite.profile))?)
        .map_err(std::io::Error::other)?;
    if profile_sha256(&profile) != item.profile_sha256 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "pinned suite profile differs from the collected profile",
        ));
    }
    if item.review.expected_observations.is_empty() {
        item.review.expected_observations = item
            .observations
            .iter()
            .filter_map(|(name, observation)| {
                expected_from_observation(observation).map(|expected| (name.clone(), expected))
            })
            .collect();
    }
    if item.review.expected_observations.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "no trusted expected observations; correct the item before promotion",
        ));
    }
    let source = current_root.join(&item.frame.image);
    let image = fs::read(&source)?;
    if image_sha256(&image) != item.image_sha256 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "collected image hash mismatch",
        ));
    }
    let media_relative = PathBuf::from(format!("media/collected/{}.png", item.id));
    let case_relative = PathBuf::from(format!("cases/collected-{}.json", item.id));
    let media_path = root.join(&media_relative);
    let case_path = root.join(&case_relative);
    fs::create_dir_all(media_path.parent().unwrap_or(root))?;
    fs::create_dir_all(case_path.parent().unwrap_or(root))?;
    atomic_write(&media_path, &image)?;
    let case = RegressionCase {
        schema: 1,
        id: format!("collected-{}", item.id),
        purpose: format!("Reviewed passive capture {}", item.created_at),
        categories: vec!["passive_collection".into(), "full_frame".into()],
        source_media: Some(media_relative.clone()),
        frames: vec![SuiteFrame {
            image: media_relative.clone(),
            timestamp_ms: 0,
            placement: FramePlacement::FullFrame,
            expected_observations: item.review.expected_observations.clone(),
        }],
        check_events: false,
        expected_events: Vec::new(),
    };
    let case_bytes = serde_json::to_vec_pretty(&case).map_err(std::io::Error::other)?;
    atomic_write(&case_path, &case_bytes)?;
    if !suite.cases.contains(&case_relative) {
        suite.cases.push(case_relative.clone());
    }
    suite
        .files
        .retain(|file| file.path != media_relative && file.path != case_relative);
    suite.files.push(SuiteFile {
        path: media_relative,
        sha256: image_sha256(&image),
    });
    suite.files.push(SuiteFile {
        path: case_relative.clone(),
        sha256: image_sha256(&case_bytes),
    });
    suite
        .files
        .sort_by(|left, right| left.path.cmp(&right.path));
    atomic_write(
        &suite_path,
        &serde_json::to_vec_pretty(&suite).map_err(std::io::Error::other)?,
    )?;
    item.review.status = ReviewStatus::Promoted;
    item.review.promoted_case = Some(case_relative);
    save_reviewed_item(root, current_root, item)
}

fn validate_item_id(id: &str) -> std::io::Result<()> {
    if id.len() > 128
        || id.is_empty()
        || !id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.'
        })
        || matches!(id, "." | "..")
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid collection item id",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use yash_app_events_capture::{FrameLayout, PixelFormat};
    use yash_app_events_profile::{DetectorId, ElementId};

    fn frame(sequence: u64, changed: bool) -> Frame {
        let mut pixels = vec![40_u8; 64 * 36 * 4];
        if changed {
            for pixel in pixels.chunks_exact_mut(4).take(16) {
                pixel[0] = 80;
            }
        }
        Frame::new(
            sequence,
            Duration::from_secs(sequence),
            FrameLayout {
                width: 64,
                height: 36,
                row_stride: 64 * 4,
                format: PixelFormat::Rgba8,
            },
            Some("test".into()),
            Arc::from(pixels),
        )
        .unwrap()
    }

    #[test]
    fn static_thumbnail_pair_is_similar_but_not_identical() {
        let first = thumbnail(&frame(1, false));
        let second = thumbnail(&frame(2, true));
        let score = similarity(&first, &second).unwrap();
        assert!(score > 0.0);
        assert!(score < 0.015);
    }

    #[test]
    fn runtime_defaults_to_disabled_and_seventy_seconds() {
        let mut runtime = Runtime::default();
        runtime.configure("profile".into(), CollectionPolicy::default());
        assert!(!runtime.due(Instant::now(), 1));
        assert_eq!(runtime.policy.unwrap().interval_seconds, 70);
    }

    #[test]
    fn reviewed_item_promotes_into_case_media_and_suite_inventory() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::create_dir_all(root.join("profile")).unwrap();
        let profile = Profile::new("Demo", "demo_game", 64, 36);
        atomic_write(
            &root.join("profile/profile.json"),
            &serde_json::to_vec_pretty(&profile).unwrap(),
        )
        .unwrap();
        let suite = RegressionSuite {
            schema: 1,
            id: "demo".into(),
            name: "Demo".into(),
            game: "demo_game".into(),
            profile: "profile/profile.json".into(),
            cases: Vec::new(),
            files: Vec::new(),
        };
        atomic_write(
            &root.join("suite.json"),
            &serde_json::to_vec_pretty(&suite).unwrap(),
        )
        .unwrap();
        let observation = Observation {
            detector_id: DetectorId::new(),
            element_id: ElementId::new(),
            timestamp_ms: 10,
            value: ObservationValue::Text("5".into()),
            confidence: Some(0.95),
            status: ObservationStatus::Valid,
            diagnostic: "test".into(),
        };
        let png = b"fixture";
        let mut item = CollectionItem {
            schema: 1,
            id: "sample-1".into(),
            created_at: "2026-07-14T00:00:00Z".into(),
            game: "demo_game".into(),
            profile_id: profile.id.to_string(),
            profile_revision: profile.revision,
            profile_sha256: profile_sha256(&profile),
            frame: yash_app_events_engine::collection::CollectedFrame {
                sequence: 1,
                timestamp_ms: 10,
                width: 64,
                height: 36,
                pixel_format: "rgba8".into(),
                source: Some("test".into()),
                image: "frame.png".into(),
            },
            image_sha256: image_sha256(png),
            observations: BTreeMap::from([("stage_group".into(), observation)]),
            transitions: Vec::new(),
            reason: yash_app_events_engine::collection::CollectionReason {
                trigger: "interval".into(),
                scene_difference: None,
                scene_novel: true,
                evidence_novel: true,
                evidence_reasons: vec!["scene_novel".into()],
            },
            review: yash_app_events_engine::collection::ReviewRecord::default(),
            thumbnail: vec![10; THUMBNAIL_WIDTH * THUMBNAIL_HEIGHT],
        };
        let current = persist_item(root, &item, png).unwrap();
        promote_item(root, &current, &mut item).unwrap();
        let updated: RegressionSuite =
            serde_json::from_slice(&fs::read(root.join("suite.json")).unwrap()).unwrap();
        assert_eq!(updated.cases.len(), 1);
        assert!(root.join(&updated.cases[0]).exists());
        assert!(root.join("promoted/sample-1/metadata.json").exists());
        assert_eq!(item.review.status, ReviewStatus::Promoted);
    }
}
