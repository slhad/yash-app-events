//! Schema and validation for read-only profile routing bundles.

use std::fs::{self, File};
use std::io::{self, BufReader, Read as _};
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{OverlayId, ProfileId, SceneId};

/// Current profile-bundle schema version.
pub const PROFILE_BUNDLE_SCHEMA_VERSION: u16 = 1;

const MAXIMUM_BUNDLE_BYTES: u64 = 256 * 1024;
const MAXIMUM_BUNDLE_MEMBERS: usize = 32;
const MAXIMUM_ROUTE_SCENES: usize = 128;
const MAXIMUM_ROUTE_OVERLAYS: usize = 128;
const MAXIMUM_BUNDLE_NAME_BYTES: usize = 128;

/// Read-only routing manifest that maps a lightweight entry profile to
/// independently validated specialized profiles.
///
/// `scene_ids` and `overlay_ids` in each member refer to the stable IDs in the
/// router profile, not to IDs in the member profile. A member with both lists
/// empty is not meaningful unless it is the single fallback member.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileBundle {
    pub schema: u16,
    pub name: String,
    /// Stable game slug shared by the router and every member.
    pub game: String,
    pub router_profile_id: ProfileId,
    pub router_profile_revision: u64,
    pub members: Vec<ProfileBundleMember>,
}

/// One specialized profile and the router context that selects it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileBundleMember {
    pub profile_id: ProfileId,
    pub profile_revision: u64,
    #[serde(default)]
    pub scene_ids: Vec<SceneId>,
    #[serde(default)]
    pub overlay_ids: Vec<OverlayId>,
    /// Higher priorities win when more than one route matches.
    #[serde(default)]
    pub priority: i16,
    /// Used only when no constrained member matches.
    #[serde(default)]
    pub fallback: bool,
}

impl ProfileBundle {
    /// Validates the bounded, non-executable routing document.
    ///
    /// # Errors
    ///
    /// Returns a structured bundle error when the schema or route shape is
    /// invalid. Referenced profiles and router IDs are validated by the daemon
    /// after loading the local profile store.
    pub fn validate(&self) -> Result<(), ProfileBundleError> {
        if self.schema != PROFILE_BUNDLE_SCHEMA_VERSION {
            return Err(ProfileBundleError::UnsupportedSchema(self.schema));
        }
        if self.name.trim().is_empty() {
            return Err(ProfileBundleError::Validation(
                "name must not be empty".into(),
            ));
        }
        if self.name.len() > MAXIMUM_BUNDLE_NAME_BYTES {
            return Err(ProfileBundleError::Validation(format!(
                "name exceeds {MAXIMUM_BUNDLE_NAME_BYTES} bytes"
            )));
        }
        if self.game.trim().is_empty() {
            return Err(ProfileBundleError::Validation(
                "game must not be empty".into(),
            ));
        }
        if self.members.is_empty() {
            return Err(ProfileBundleError::Validation(
                "members must contain at least one profile".into(),
            ));
        }
        if self.members.len() > MAXIMUM_BUNDLE_MEMBERS {
            return Err(ProfileBundleError::Validation(format!(
                "members exceeds the limit of {MAXIMUM_BUNDLE_MEMBERS}"
            )));
        }

        let mut profile_ids = std::collections::HashSet::new();
        let mut fallback_count = 0_usize;
        for (index, member) in self.members.iter().enumerate() {
            if !profile_ids.insert(member.profile_id) {
                return Err(ProfileBundleError::Validation(format!(
                    "members[{index}].profile_id is duplicated"
                )));
            }
            if member.scene_ids.len() > MAXIMUM_ROUTE_SCENES {
                return Err(ProfileBundleError::Validation(format!(
                    "members[{index}].scene_ids exceeds the limit of {MAXIMUM_ROUTE_SCENES}"
                )));
            }
            if member.overlay_ids.len() > MAXIMUM_ROUTE_OVERLAYS {
                return Err(ProfileBundleError::Validation(format!(
                    "members[{index}].overlay_ids exceeds the limit of {MAXIMUM_ROUTE_OVERLAYS}"
                )));
            }
            if has_duplicates(&member.scene_ids) {
                return Err(ProfileBundleError::Validation(format!(
                    "members[{index}].scene_ids contains duplicate IDs"
                )));
            }
            if has_duplicates(&member.overlay_ids) {
                return Err(ProfileBundleError::Validation(format!(
                    "members[{index}].overlay_ids contains duplicate IDs"
                )));
            }
            if member.fallback {
                fallback_count = fallback_count.saturating_add(1);
                if !member.scene_ids.is_empty() || !member.overlay_ids.is_empty() {
                    return Err(ProfileBundleError::Validation(format!(
                        "members[{index}] fallback must not contain scene_ids or overlay_ids"
                    )));
                }
            } else if member.scene_ids.is_empty() && member.overlay_ids.is_empty() {
                return Err(ProfileBundleError::Validation(format!(
                    "members[{index}] needs scene_ids, overlay_ids, or fallback=true"
                )));
            }
        }
        if fallback_count > 1 {
            return Err(ProfileBundleError::Validation(
                "members may contain at most one fallback".into(),
            ));
        }
        Ok(())
    }
}

/// Loads and validates one bounded local routing manifest.
///
/// The file contains only IDs and routing metadata. It cannot name executable
/// content or arbitrary profile paths; profile directories are derived by the
/// daemon from the referenced IDs.
///
/// # Errors
///
/// Returns I/O, JSON, schema, or bounded-validation errors without modifying
/// the source file.
pub fn load_profile_bundle(path: &Path) -> Result<ProfileBundle, ProfileBundleError> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(ProfileBundleError::Validation(
            "bundle path must name a regular file".into(),
        ));
    }
    if metadata.len() > MAXIMUM_BUNDLE_BYTES {
        return Err(ProfileBundleError::LimitExceeded {
            limit: MAXIMUM_BUNDLE_BYTES,
        });
    }
    let mut reader = BufReader::new(File::open(path)?).take(MAXIMUM_BUNDLE_BYTES + 1);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAXIMUM_BUNDLE_BYTES {
        return Err(ProfileBundleError::LimitExceeded {
            limit: MAXIMUM_BUNDLE_BYTES,
        });
    }
    let bundle: ProfileBundle = serde_json::from_slice(&bytes)?;
    bundle.validate()?;
    Ok(bundle)
}

fn has_duplicates<T>(values: &[T]) -> bool
where
    T: Eq + std::hash::Hash + Copy,
{
    let mut seen = std::collections::HashSet::with_capacity(values.len());
    values.iter().copied().any(|value| !seen.insert(value))
}

/// Failure while reading or validating a profile routing manifest.
#[derive(Debug, Error)]
pub enum ProfileBundleError {
    #[error("profile bundle I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("profile bundle JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported profile bundle schema {0}")]
    UnsupportedSchema(u16),
    #[error("invalid profile bundle: {0}")]
    Validation(String),
    #[error("profile bundle exceeds the {limit}-byte size limit")]
    LimitExceeded { limit: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_bundle() -> ProfileBundle {
        ProfileBundle {
            schema: PROFILE_BUNDLE_SCHEMA_VERSION,
            name: "Queen control".into(),
            game: "queens_blade_limit_break".into(),
            router_profile_id: ProfileId::new(),
            router_profile_revision: 3,
            members: vec![
                ProfileBundleMember {
                    profile_id: ProfileId::new(),
                    profile_revision: 7,
                    scene_ids: vec![SceneId::new()],
                    overlay_ids: vec![OverlayId::new()],
                    priority: 10,
                    fallback: false,
                },
                ProfileBundleMember {
                    profile_id: ProfileId::new(),
                    profile_revision: 8,
                    scene_ids: Vec::new(),
                    overlay_ids: Vec::new(),
                    priority: 0,
                    fallback: true,
                },
            ],
        }
    }

    #[test]
    fn valid_bundle_round_trips() {
        let bundle = valid_bundle();
        let bytes = serde_json::to_vec(&bundle).unwrap();
        let decoded: ProfileBundle = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, bundle);
        decoded.validate().unwrap();
    }

    #[test]
    fn bundle_rejects_duplicate_profiles_and_routes() {
        let mut bundle = valid_bundle();
        bundle.members[1].profile_id = bundle.members[0].profile_id;
        assert!(matches!(
            bundle.validate(),
            Err(ProfileBundleError::Validation(message)) if message.contains("profile_id is duplicated")
        ));

        let mut bundle = valid_bundle();
        let scene_id = bundle.members[0].scene_ids[0];
        bundle.members[0].scene_ids.push(scene_id);
        assert!(matches!(
            bundle.validate(),
            Err(ProfileBundleError::Validation(message)) if message.contains("scene_ids contains duplicate")
        ));
    }

    #[test]
    fn bundle_rejects_multiple_or_constrained_fallbacks() {
        let mut bundle = valid_bundle();
        bundle.members[0].fallback = true;
        assert!(matches!(
            bundle.validate(),
            Err(ProfileBundleError::Validation(message)) if message.contains("fallback must not")
        ));

        let mut bundle = valid_bundle();
        bundle.members.push(ProfileBundleMember {
            profile_id: ProfileId::new(),
            profile_revision: 9,
            scene_ids: Vec::new(),
            overlay_ids: Vec::new(),
            priority: -1,
            fallback: true,
        });
        assert!(matches!(
            bundle.validate(),
            Err(ProfileBundleError::Validation(message)) if message.contains("at most one fallback")
        ));
    }

    #[test]
    fn load_bundle_rejects_oversized_documents_without_parsing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bundle.json");
        let size = usize::try_from(MAXIMUM_BUNDLE_BYTES)
            .unwrap()
            .saturating_add(1);
        fs::write(&path, vec![b' '; size]).unwrap();
        assert!(matches!(
            load_profile_bundle(&path),
            Err(ProfileBundleError::LimitExceeded {
                limit: MAXIMUM_BUNDLE_BYTES
            })
        ));
    }
}
