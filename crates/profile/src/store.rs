use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use yash_app_events_output::OutputRecipe;

use crate::{save_profile, ElementId, Profile, ProfileId, RuleId, RulePredicate, StorageError};

const PROFILE_FILE: &str = "profile.json";
const DRAFT_FILE: &str = "draft.json";
const OUTPUT_RECIPES_DIRECTORY: &str = "output-recipes";
const MAXIMUM_OUTPUT_RECIPES: usize = 32;
const MAXIMUM_OUTPUT_RECIPE_BYTES: u64 = 64 * 1024;

/// Validated portable recipe plus integrity metadata shown during local installation.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OutputRecipeEntry {
    pub path: String,
    pub sha256: String,
    pub recipe: OutputRecipe,
}

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

    /// Lists every retained committed revision, including the current profile.
    ///
    /// # Errors
    ///
    /// Returns parsing, validation, or storage errors.
    pub fn list_revisions(&self, id: ProfileId) -> Result<Vec<Profile>, StoreError> {
        let current = self.load(id)?;
        let history = self.profile_directory(id).join("revisions");
        let mut revisions = Vec::new();
        if history.exists() {
            for entry in fs::read_dir(history)? {
                let path = entry?.path();
                if path.extension().and_then(|value| value.to_str()) == Some("json") {
                    revisions.push(crate::load_profile(&path)?);
                }
            }
        }
        revisions.push(current);
        revisions.sort_by_key(|profile| profile.revision);
        revisions.dedup_by_key(|profile| profile.revision);
        Ok(revisions)
    }

    /// Loads one retained committed revision.
    ///
    /// # Errors
    ///
    /// Returns `RevisionNotFound` when the revision was pruned or never existed.
    pub fn load_revision(&self, id: ProfileId, revision: u64) -> Result<Profile, StoreError> {
        let current = self.load(id)?;
        if current.revision == revision {
            return Ok(current);
        }
        let path = self
            .profile_directory(id)
            .join("revisions")
            .join(format!("{revision}.json"));
        if !path.is_file() {
            return Err(StoreError::RevisionNotFound { id, revision });
        }
        Ok(crate::load_profile(&path)?)
    }

    /// Rolls a retained snapshot forward as a new committed revision.
    ///
    /// # Errors
    ///
    /// Returns revision conflicts, missing history, validation, or storage errors.
    pub fn rollback(
        &self,
        id: ProfileId,
        revision: u64,
        expected_revision: u64,
    ) -> Result<Profile, StoreError> {
        let snapshot = self.load_revision(id, revision)?;
        self.commit(snapshot, expected_revision)
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
        for derived in &mut duplicate.derived_observations {
            let old = derived.id;
            derived.id = ElementId::new();
            element_ids.insert(old, derived.id);
            for input in &mut derived.inputs {
                if let Some(id) = element_ids.get(&input.element_id) {
                    input.element_id = *id;
                }
            }
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

    /// Lists validated inert output recipes carried by one portable profile.
    ///
    /// # Errors
    ///
    /// Returns errors for missing profiles, links/non-files, excessive files or sizes,
    /// malformed JSON, duplicate recipe IDs, or invalid recipe contracts.
    pub fn output_recipes(&self, id: ProfileId) -> Result<Vec<OutputRecipeEntry>, StoreError> {
        self.load(id)?;
        load_output_recipes_from_profile_directory(&self.profile_directory(id))
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

pub(crate) fn load_output_recipes_from_profile_directory(
    profile_directory: &Path,
) -> Result<Vec<OutputRecipeEntry>, StoreError> {
    let directory = profile_directory.join(OUTPUT_RECIPES_DIRECTORY);
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let directory_type = fs::symlink_metadata(&directory)?.file_type();
    if !directory_type.is_dir() || directory_type.is_symlink() {
        return Err(StoreError::InvalidOutputRecipe(
            "output-recipes must be a regular directory".into(),
        ));
    }
    let mut entries = Vec::new();
    let mut ids = HashSet::new();
    for entry in fs::read_dir(directory)? {
        if entries.len() >= MAXIMUM_OUTPUT_RECIPES {
            return Err(StoreError::InvalidOutputRecipe(
                "a profile may carry at most 32 output recipes".into(),
            ));
        }
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_file() || file_type.is_symlink() {
            return Err(StoreError::InvalidOutputRecipe(
                "output recipe entries must be regular files".into(),
            ));
        }
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            return Err(StoreError::InvalidOutputRecipe(
                "output recipe files must use the .json extension".into(),
            ));
        }
        let file_name = entry.file_name().into_string().map_err(|_| {
            StoreError::InvalidOutputRecipe("output recipe filenames must be UTF-8".into())
        })?;
        let metadata = entry.metadata()?;
        if metadata.len() > MAXIMUM_OUTPUT_RECIPE_BYTES {
            return Err(StoreError::InvalidOutputRecipe(
                "an output recipe must not exceed 64 KiB".into(),
            ));
        }
        let bytes = fs::read(&path)?;
        let recipe: OutputRecipe = serde_json::from_slice(&bytes).map_err(|error| {
            StoreError::InvalidOutputRecipe(format!("{}: {error}", path.display()))
        })?;
        recipe.validate().map_err(|error| {
            StoreError::InvalidOutputRecipe(format!("{}: {error}", path.display()))
        })?;
        if !ids.insert(recipe.id) {
            return Err(StoreError::InvalidOutputRecipe(
                "output recipe IDs must be unique within a profile".into(),
            ));
        }
        entries.push(OutputRecipeEntry {
            path: format!("{OUTPUT_RECIPES_DIRECTORY}/{file_name}"),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            recipe,
        });
    }
    entries.sort_by(|left, right| left.recipe.name.cmp(&right.recipe.name));
    Ok(entries)
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
        | RulePredicate::TextContains { .. }
        | RulePredicate::RapidIncrease { .. } => return,
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
    #[error("profile {id} revision {revision} was not found")]
    RevisionNotFound { id: ProfileId, revision: u64 },
    #[error("invalid portable output recipe: {0}")]
    InvalidOutputRecipe(String),
}

#[cfg(test)]
mod tests {
    use crate::{
        AtomicRulePredicate, BarDirection, Detector, DetectorId, Element, EventRule,
        NormalizedRegion, ObservationCondition, RulePredicate,
    };

    use super::*;
    use serde_json::json;
    use yash_app_events_output::{
        EventState, OutputFormat, OutputRecipe, OutputRecipeSink, OutputTrigger,
    };

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
    fn portable_output_recipes_are_validated_hashed_and_sorted() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(directory.path(), 20);
        let profile = Profile::new("Recipes", "demo_game", 1920, 1080);
        store.create(&profile).unwrap();
        let recipes = store.profile_directory(profile.id).join("output-recipes");
        fs::create_dir_all(&recipes).unwrap();
        let recipe = OutputRecipe {
            schema: 1,
            id: uuid::Uuid::new_v4(),
            name: "Stage marker".into(),
            description: "Inert example".into(),
            trigger: OutputTrigger::Event {
                events: vec!["stage_changed".into()],
                states: vec![EventState::Updated],
            },
            format: OutputFormat::JsonTemplate {
                template: json!({"stage":"{{event.value}}"}),
            },
            suggested_sink: OutputRecipeSink::Command {
                program_name: "yash".into(),
                args: vec!["ipc".into(), "command".into(), "marker".into()],
                timeout_ms: 5_000,
            },
        };
        fs::write(
            recipes.join("yash-stage-marker.json"),
            serde_json::to_vec_pretty(&recipe).unwrap(),
        )
        .unwrap();
        let entries = store.output_recipes(profile.id).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].recipe, recipe);
        assert_eq!(entries[0].path, "output-recipes/yash-stage-marker.json");
        assert_eq!(entries[0].sha256.len(), 64);
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
    fn rollback_preserves_history_and_creates_new_revision() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(directory.path(), 10);
        let profile = populated_profile();
        let id = profile.id;
        store.create(&profile).unwrap();
        let mut first = profile.clone();
        first.name = "Changed".into();
        let first = store.commit(first, 0).unwrap();

        let rolled_back = store.rollback(id, 0, first.revision).unwrap();
        assert_eq!(rolled_back.revision, 2);
        assert_eq!(rolled_back.name, "Demo");
        assert_eq!(
            store
                .list_revisions(id)
                .unwrap()
                .iter()
                .map(|profile| profile.revision)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
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
