use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::{save_profile, ElementId, Profile, ProfileId, RuleId, RulePredicate, StorageError};

const PROFILE_FILE: &str = "profile.json";
const DRAFT_FILE: &str = "draft.json";

/// State-owning profile repository used only by the daemon.
#[derive(Debug)]
pub struct ProfileStore {
    profiles: PathBuf,
    trash: PathBuf,
    revision_limit: usize,
}

impl ProfileStore {
    /// Creates a store rooted in the application's data directory.
    #[must_use]
    pub fn new(data_root: impl Into<PathBuf>, revision_limit: usize) -> Self {
        let root = data_root.into();
        Self {
            profiles: root.join("profiles"),
            trash: root.join("trash"),
            revision_limit: revision_limit.max(1),
        }
    }

    /// Creates and commits a new profile.
    ///
    /// # Errors
    ///
    /// Returns validation, collision, or durable storage errors.
    pub fn create(&self, profile: &Profile) -> Result<(), StoreError> {
        let directory = self.profile_directory(profile.id);
        if directory.exists() {
            return Err(StoreError::AlreadyExists(profile.id));
        }
        save_profile(&directory.join(PROFILE_FILE), profile)?;
        Ok(())
    }

    /// Loads a committed profile.
    ///
    /// # Errors
    ///
    /// Returns parsing, validation, or storage errors.
    pub fn load(&self, id: ProfileId) -> Result<Profile, StoreError> {
        Ok(crate::load_profile(
            &self.profile_directory(id).join(PROFILE_FILE),
        )?)
    }

    /// Lists committed profiles in stable identity order.
    ///
    /// # Errors
    ///
    /// Returns an error if any stored profile cannot be loaded safely.
    pub fn list(&self) -> Result<Vec<Profile>, StoreError> {
        if !self.profiles.exists() {
            return Ok(Vec::new());
        }
        let mut profiles = Vec::new();
        for entry in fs::read_dir(&self.profiles)? {
            let path = entry?.path().join(PROFILE_FILE);
            if path.is_file() {
                profiles.push(crate::load_profile(&path)?);
            }
        }
        profiles.sort_by_key(|profile| profile.id.to_string());
        Ok(profiles)
    }

    /// Commits a mutation if its expected revision is current.
    ///
    /// # Errors
    ///
    /// Returns a structured conflict without overwriting newer work, or a storage error.
    pub fn commit(
        &self,
        mut profile: Profile,
        expected_revision: u64,
    ) -> Result<Profile, StoreError> {
        let current = self.load(profile.id)?;
        if current.revision != expected_revision {
            return Err(StoreError::RevisionConflict {
                expected: expected_revision,
                current: current.revision,
            });
        }
        let directory = self.profile_directory(profile.id);
        let history = directory.join("revisions");
        fs::create_dir_all(&history)?;
        save_profile(
            &history.join(format!("{}.json", current.revision)),
            &current,
        )?;
        profile.revision = current.revision.saturating_add(1);
        save_profile(&directory.join(PROFILE_FILE), &profile)?;
        self.prune_revisions(&history)?;
        let draft = directory.join(DRAFT_FILE);
        if draft.exists() {
            fs::remove_file(draft)?;
        }
        Ok(profile)
    }

    /// Atomically saves a recoverable draft without changing the committed revision.
    ///
    /// # Errors
    ///
    /// Returns validation or durable storage errors.
    pub fn save_draft(&self, profile: &Profile) -> Result<(), StoreError> {
        save_profile(
            &self.profile_directory(profile.id).join(DRAFT_FILE),
            profile,
        )?;
        Ok(())
    }

    /// Deep-copies a portable profile and all portable assets with fresh internal IDs.
    ///
    /// # Errors
    ///
    /// Returns an error if source assets or the new profile cannot be copied safely.
    pub fn duplicate_profile(
        &self,
        source_id: ProfileId,
        name: impl Into<String>,
    ) -> Result<Profile, StoreError> {
        let source = self.load(source_id)?;
        let mut duplicate = source.clone();
        duplicate.id = ProfileId::new();
        duplicate.revision = 0;
        duplicate.name = name.into();
        let mut element_ids = HashMap::new();
        for element in &mut duplicate.elements {
            let old = element.id;
            element.id = ElementId::new();
            element.detector.assign_new_id();
            element_ids.insert(old, element.id);
        }
        for rule in &mut duplicate.rules {
            rule.id = RuleId::new();
            if let Some(id) = element_ids.get(&rule.element_id) {
                rule.element_id = *id;
            }
            remap_composition_elements(rule, |id| element_ids.get(&id).copied());
        }
        let source_directory = self.profile_directory(source_id);
        let destination = self.profile_directory(duplicate.id);
        copy_portable_tree(&source_directory, &destination)?;
        let revisions = destination.join("revisions");
        if revisions.exists() {
            fs::remove_dir_all(revisions)?;
        }
        let draft = destination.join(DRAFT_FILE);
        if draft.exists() {
            fs::remove_file(draft)?;
        }
        save_profile(&destination.join(PROFILE_FILE), &duplicate)?;
        Ok(duplicate)
    }

    /// Duplicates an element; rules remain unchanged unless explicitly requested.
    ///
    /// # Errors
    ///
    /// Returns an error if the requested element is absent.
    pub fn duplicate_element(
        profile: &mut Profile,
        element_id: ElementId,
        copy_rules: bool,
    ) -> Result<ElementId, StoreError> {
        let original = profile
            .elements
            .iter()
            .find(|element| element.id == element_id)
            .cloned()
            .ok_or(StoreError::ElementNotFound(element_id))?;
        let mut duplicate = original;
        duplicate.id = ElementId::new();
        duplicate.detector.assign_new_id();
        let new_id = duplicate.id;
        profile.elements.push(duplicate);
        if copy_rules {
            let copied: Vec<_> = profile
                .rules
                .iter()
                .filter(|rule| rule.element_id == element_id)
                .cloned()
                .map(|mut rule| {
                    rule.id = RuleId::new();
                    rule.element_id = new_id;
                    remap_composition_elements(&mut rule, |id| {
                        (id == element_id).then_some(new_id)
                    });
                    rule
                })
                .collect();
            profile.rules.extend(copied);
        }
        Ok(new_id)
    }

    /// Moves a profile to application-managed trash.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the move cannot complete.
    pub fn trash(&self, id: ProfileId) -> Result<(), StoreError> {
        fs::create_dir_all(&self.trash)?;
        fs::rename(self.profile_directory(id), self.trash.join(id.to_string()))?;
        Ok(())
    }

    /// Restores a trashed profile, refusing to replace an existing profile.
    ///
    /// # Errors
    ///
    /// Returns a collision or I/O error.
    pub fn restore(&self, id: ProfileId) -> Result<(), StoreError> {
        let destination = self.profile_directory(id);
        if destination.exists() {
            return Err(StoreError::AlreadyExists(id));
        }
        fs::create_dir_all(&self.profiles)?;
        fs::rename(self.trash.join(id.to_string()), destination)?;
        Ok(())
    }

    /// Permanently removes an already-trashed profile.
    ///
    /// # Errors
    ///
    /// Returns an I/O error; active profiles are never accepted by this trash-only path.
    pub fn permanently_delete_trashed(&self, id: ProfileId) -> Result<(), StoreError> {
        fs::remove_dir_all(self.trash.join(id.to_string()))?;
        Ok(())
    }

    /// Returns the portable directory for a stable profile ID.
    #[must_use]
    pub fn profile_directory(&self, id: ProfileId) -> PathBuf {
        self.profiles.join(id.to_string())
    }

    /// Returns the root containing all portable profiles.
    #[must_use]
    pub fn profiles_root(&self) -> &Path {
        &self.profiles
    }

    fn prune_revisions(&self, history: &Path) -> io::Result<()> {
        let mut revisions: Vec<_> = fs::read_dir(history)?.collect::<Result<_, _>>()?;
        revisions.sort_by_key(|entry| {
            entry
                .path()
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| stem.parse::<u64>().ok())
                .unwrap_or(u64::MAX)
        });
        let remove_count = revisions.len().saturating_sub(self.revision_limit);
        for entry in revisions.into_iter().take(remove_count) {
            fs::remove_file(entry.path())?;
        }
        Ok(())
    }
}

fn remap_composition_elements(
    rule: &mut crate::EventRule,
    mut replacement: impl FnMut(ElementId) -> Option<ElementId>,
) {
    let conditions = match &mut rule.predicate {
        RulePredicate::All { conditions } | RulePredicate::Any { conditions } => conditions,
        RulePredicate::NumericBelow
        | RulePredicate::Boolean { .. }
        | RulePredicate::TextEquals { .. }
        | RulePredicate::TextContains { .. } => return,
    };
    for condition in conditions {
        if let Some(id) = replacement(condition.element_id) {
            condition.element_id = id;
        }
    }
}

fn copy_portable_tree(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    let excluded = HashSet::from([PROFILE_FILE, DRAFT_FILE, "revisions"]);
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if excluded.contains(name.to_string_lossy().as_ref()) {
            continue;
        }
        let target = destination.join(&name);
        if entry.file_type()?.is_dir() {
            copy_portable_tree(&entry.path(), &target)?;
        } else if entry.file_type()?.is_file() {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Profile repository operation failure.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("profile I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("profile {0} already exists")]
    AlreadyExists(ProfileId),
    #[error("stale profile revision: expected {expected}, current {current}")]
    RevisionConflict { expected: u64, current: u64 },
    #[error("element {0} was not found")]
    ElementNotFound(ElementId),
}

#[cfg(test)]
mod tests {
    use crate::{
        AtomicRulePredicate, BarDirection, Detector, DetectorId, Element, EventRule,
        NormalizedRegion, ObservationCondition, RulePredicate,
    };

    use super::*;

    fn populated_profile() -> Profile {
        let mut profile = Profile::new("Demo", "demo_game", 1920, 1080);
        let element_id = ElementId::new();
        profile.elements.push(Element {
            id: element_id,
            name: "Health".into(),
            enabled: true,
            color: "#f00".into(),
            region: NormalizedRegion {
                x: 0.1,
                y: 0.1,
                width: 0.5,
                height: 0.1,
            },
            detector: Detector::ColorBar {
                id: DetectorId::new(),
                direction: BarDirection::LeftToRight,
                minimum_rgb: [120, 0, 0],
                maximum_rgb: [255, 80, 80],
                mask: None,
            },
        });
        profile.rules.push(EventRule {
            id: RuleId::new(),
            element_id,
            event: "critical_health".into(),
            enter_below: 0.2,
            leave_above: 0.3,
            minimum_confidence: 0.8,
            required_samples: 2,
            sample_window: 3,
            cooldown_ms: 500,
            predicate: RulePredicate::default(),
            stable_for_ms: 0,
            emit_initial: false,
            update_interval_ms: None,
        });
        profile
    }

    #[test]
    fn stale_commit_reports_current_revision_without_overwrite() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(directory.path(), 2);
        let profile = populated_profile();
        store.create(&profile).unwrap();
        let committed = store.commit(profile.clone(), 0).unwrap();
        let error = store.commit(profile, 0).unwrap_err();
        assert!(matches!(
            error,
            StoreError::RevisionConflict {
                expected: 0,
                current: 1
            }
        ));
        assert_eq!(store.load(committed.id).unwrap().revision, 1);
    }

    #[test]
    fn deep_duplicate_rekeys_objects_copies_assets_and_resets_history() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(directory.path(), 20);
        let mut profile = populated_profile();
        profile.rules[0].predicate = RulePredicate::All {
            conditions: vec![ObservationCondition {
                element_id: profile.elements[0].id,
                predicate: AtomicRulePredicate::Boolean { expected: true },
            }],
        };
        store.create(&profile).unwrap();
        let source = store.profile_directory(profile.id);
        fs::create_dir_all(source.join("templates")).unwrap();
        fs::write(source.join("templates/bar.bin"), b"asset").unwrap();
        let duplicate = store.duplicate_profile(profile.id, "Copy").unwrap();
        assert_ne!(duplicate.id, profile.id);
        assert_ne!(duplicate.elements[0].id, profile.elements[0].id);
        assert_eq!(duplicate.rules[0].element_id, duplicate.elements[0].id);
        let RulePredicate::All { conditions } = &duplicate.rules[0].predicate else {
            panic!("composition was not preserved");
        };
        assert_eq!(conditions[0].element_id, duplicate.elements[0].id);
        assert_eq!(duplicate.revision, 0);
        assert_eq!(
            fs::read(
                store
                    .profile_directory(duplicate.id)
                    .join("templates/bar.bin")
            )
            .unwrap(),
            b"asset"
        );
    }

    #[test]
    fn trash_and_restore_are_reversible() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(directory.path(), 20);
        let profile = populated_profile();
        store.create(&profile).unwrap();
        store.trash(profile.id).unwrap();
        assert!(store.load(profile.id).is_err());
        store.restore(profile.id).unwrap();
        assert_eq!(store.load(profile.id).unwrap(), profile);
    }

    #[test]
    fn element_rules_are_copied_only_when_requested() {
        let mut profile = populated_profile();
        let original = profile.elements[0].id;
        ProfileStore::duplicate_element(&mut profile, original, false).unwrap();
        assert_eq!(profile.rules.len(), 1);
        ProfileStore::duplicate_element(&mut profile, original, true).unwrap();
        assert_eq!(profile.rules.len(), 2);
    }
}
