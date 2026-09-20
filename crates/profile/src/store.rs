use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use yash_app_events_output::OutputRecipe;

use crate::{
    save_profile, DetectorId, ElementId, ImportLimits, InteractionTarget, OverlayId, Profile,
    ProfileId, RecognitionExpression, RuleId, RulePredicate, SceneId, StorageError,
};

const PROFILE_FILE: &str = "profile.json";
const DRAFT_FILE: &str = "draft.json";
const REVISION_LINEAGE_FILE: &str = ".revision-lineage.json";
const REVISION_LINEAGE_SCHEMA: u16 = 1;
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

#[derive(Debug, Deserialize, Serialize)]
struct RevisionLineage {
    schema: u16,
    revisions: BTreeMap<String, u64>,
}

impl Default for RevisionLineage {
    fn default() -> Self {
        Self {
            schema: REVISION_LINEAGE_SCHEMA,
            revisions: BTreeMap::new(),
        }
    }
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
        if directory.exists() || self.trash_directory(profile.id).exists() {
            return Err(StoreError::AlreadyExists(profile.id));
        }
        if let Some(floor) = self.import_revision_floor(profile.id)? {
            if profile.revision <= floor {
                return Err(StoreError::RevisionRegression {
                    id: profile.id,
                    revision: profile.revision,
                    floor,
                });
            }
        }
        save_profile(&directory.join(PROFILE_FILE), profile)?;
        self.record_revision(profile.id, profile.revision)?;
        Ok(())
    }

    /// Imports a portable archive through the daemon-owned store boundary.
    ///
    /// A fresh profile ID keeps the archive's revision. When the same stable ID
    /// has already existed locally, the imported document is rebased to the next
    /// revision after the local high-water mark. This preserves monotonic
    /// revisions even when the previous directory was moved to trash or an
    /// external backup before the import.
    ///
    /// # Errors
    ///
    /// Returns archive, collision, lineage, validation, or durable storage errors.
    pub fn import_archive(
        &self,
        archive_path: &Path,
        limits: ImportLimits,
    ) -> Result<Profile, StoreError> {
        fs::create_dir_all(&self.profiles)?;
        let staging = self
            .profiles
            .join(format!(".import-lineage-{}.tmp", Uuid::new_v4()));
        fs::create_dir(&staging)?;
        let result = (|| {
            let mut profile = crate::import_profile(archive_path, &staging, limits)?;
            let destination = self.profile_directory(profile.id);
            if destination.exists() {
                return Err(StoreError::AlreadyExists(profile.id));
            }

            if let Some(floor) = self.import_revision_floor(profile.id)? {
                let next_revision = floor
                    .checked_add(1)
                    .ok_or(StoreError::RevisionExhausted { id: profile.id })?;
                if profile.revision < next_revision {
                    profile.revision = next_revision;
                    save_profile(
                        &staging.join(profile.id.to_string()).join(PROFILE_FILE),
                        &profile,
                    )?;
                }
            }

            self.record_revision(profile.id, profile.revision)?;
            fs::rename(staging.join(profile.id.to_string()), destination)?;
            Ok(profile)
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&staging);
        } else {
            let _ = fs::remove_dir(&staging);
        }
        result
    }

    /// Loads a committed profile.
    ///
    /// # Errors
    ///
    /// Returns parsing, validation, or storage errors.
    pub fn load(&self, id: ProfileId) -> Result<Profile, StoreError> {
        let profile = crate::load_profile(&self.profile_directory(id).join(PROFILE_FILE))?;
        if profile.id != id {
            return Err(StoreError::ProfileIdentityMismatch {
                expected: id,
                actual: profile.id,
            });
        }
        self.ensure_tracked_revision(&profile)?;
        Ok(profile)
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
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Ok(uuid) = Uuid::parse_str(&name.to_string_lossy()) else {
                continue;
            };
            let id = ProfileId(uuid);
            if entry.path() == self.profile_directory(id)
                && entry.path().join(PROFILE_FILE).is_file()
            {
                profiles.push(self.load(id)?);
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
        let next_revision = current
            .revision
            .checked_add(1)
            .ok_or(StoreError::RevisionExhausted { id: profile.id })?;
        let directory = self.profile_directory(profile.id);
        let history = directory.join("revisions");
        fs::create_dir_all(&history)?;
        save_profile(
            &history.join(format!("{}.json", current.revision)),
            &current,
        )?;
        profile.revision = next_revision;
        save_profile(&directory.join(PROFILE_FILE), &profile)?;
        self.record_revision(profile.id, profile.revision)?;
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
            derived.detector_id = DetectorId::new();
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
        duplicate.global_elements = duplicate
            .global_elements
            .iter()
            .filter_map(|id| element_ids.get(id).copied())
            .collect();
        duplicate.element_aliases = duplicate
            .element_aliases
            .iter()
            .filter_map(|(alias, id)| element_ids.get(id).copied().map(|id| (alias.clone(), id)))
            .collect();
        let scene_ids = duplicate
            .scenes
            .iter()
            .map(|scene| (scene.id, SceneId::new()))
            .collect::<HashMap<_, _>>();
        for scene in &mut duplicate.scenes {
            scene.id = scene_ids[&scene.id];
            remap_scene_elements(&mut scene.element_ids, &element_ids);
            remap_scene_elements(&mut scene.required_element_ids, &element_ids);
            remap_recognition(&mut scene.recognition, &element_ids);
            remap_interaction_targets(&mut scene.interaction_targets, &element_ids);
            scene.transition_hints = scene
                .transition_hints
                .iter()
                .filter_map(|id| scene_ids.get(id).copied())
                .collect();
        }
        for overlay in &mut duplicate.overlays {
            overlay.id = OverlayId::new();
            remap_scene_elements(&mut overlay.element_ids, &element_ids);
            remap_scene_elements(&mut overlay.required_element_ids, &element_ids);
            remap_recognition(&mut overlay.recognition, &element_ids);
            remap_interaction_targets(&mut overlay.interaction_targets, &element_ids);
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
        self.record_revision(duplicate.id, duplicate.revision)?;
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
        if profile.global_elements.contains(&element_id) {
            profile.global_elements.push(new_id);
        }
        for scene in &mut profile.scenes {
            if scene.element_ids.contains(&element_id) {
                scene.element_ids.push(new_id);
            }
            if scene.required_element_ids.contains(&element_id) {
                scene.required_element_ids.push(new_id);
            }
        }
        for overlay in &mut profile.overlays {
            if overlay.element_ids.contains(&element_id) {
                overlay.element_ids.push(new_id);
            }
            if overlay.required_element_ids.contains(&element_id) {
                overlay.required_element_ids.push(new_id);
            }
        }
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

    /// Removes a detector element and its dependent derived observations.
    ///
    /// References that cannot survive the removal are pruned from rules,
    /// layers, recognition expressions, and target visibility expressions.
    /// Affected recognition layers are disabled and affected targets hidden
    /// until the user supplies new evidence in the draft.
    /// This keeps an explicit authoring removal reversible through the draft
    /// workflow while preserving profile validation invariants.
    ///
    /// # Errors
    ///
    /// Returns an error if the requested detector element is absent.
    pub fn remove_element(profile: &mut Profile, element_id: ElementId) -> Result<(), StoreError> {
        if !profile
            .elements
            .iter()
            .any(|element| element.id == element_id)
        {
            return Err(StoreError::ElementNotFound(element_id));
        }

        let mut removed = HashSet::from([element_id]);
        loop {
            let newly_removed = profile
                .derived_observations
                .iter()
                .filter(|derived| {
                    derived
                        .inputs
                        .iter()
                        .any(|input| removed.contains(&input.element_id))
                })
                .map(|derived| derived.id)
                .filter(|id| !removed.contains(id))
                .collect::<Vec<_>>();
            if newly_removed.is_empty() {
                break;
            }
            removed.extend(newly_removed);
        }

        profile
            .elements
            .retain(|element| !removed.contains(&element.id));
        profile
            .derived_observations
            .retain(|derived| !removed.contains(&derived.id));
        profile
            .element_aliases
            .retain(|_, id| !removed.contains(id));
        profile.global_elements.retain(|id| !removed.contains(id));
        profile.rules.retain(|rule| {
            !removed.contains(&rule.element_id) && !rule_references_any(rule, &removed)
        });

        for scene in &mut profile.scenes {
            remove_element_references(&mut scene.element_ids, &removed);
            remove_element_references(&mut scene.required_element_ids, &removed);
            if remove_recognition_references(&mut scene.recognition, &removed) {
                scene.enabled = false;
            }
            remove_target_visibility_references(&mut scene.interaction_targets, &removed);
        }
        for overlay in &mut profile.overlays {
            remove_element_references(&mut overlay.element_ids, &removed);
            remove_element_references(&mut overlay.required_element_ids, &removed);
            if remove_recognition_references(&mut overlay.recognition, &removed) {
                overlay.enabled = false;
            }
            remove_target_visibility_references(&mut overlay.interaction_targets, &removed);
        }
        Ok(())
    }

    /// Moves a profile to application-managed trash.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the move cannot complete.
    pub fn trash(&self, id: ProfileId) -> Result<(), StoreError> {
        let profile = self.load(id)?;
        self.record_revision(id, profile.revision)?;
        fs::create_dir_all(&self.trash)?;
        fs::rename(self.profile_directory(id), self.trash_directory(id))?;
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
        let source = self.trash_directory(id);
        let profile = crate::load_profile(&source.join(PROFILE_FILE))?;
        if profile.id != id {
            return Err(StoreError::ProfileIdentityMismatch {
                expected: id,
                actual: profile.id,
            });
        }
        if let Some(floor) = self.import_revision_floor(id)? {
            if profile.revision < floor {
                return Err(StoreError::RevisionRegression {
                    id,
                    revision: profile.revision,
                    floor,
                });
            }
        }
        fs::create_dir_all(&self.profiles)?;
        self.record_revision(id, profile.revision)?;
        fs::rename(source, destination)?;
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

    fn trash_directory(&self, id: ProfileId) -> PathBuf {
        self.trash.join(id.to_string())
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

    fn lineage_path(&self) -> PathBuf {
        self.profiles.join(REVISION_LINEAGE_FILE)
    }

    fn read_lineage(&self) -> Result<RevisionLineage, StoreError> {
        let path = self.lineage_path();
        if !path.exists() {
            return Ok(RevisionLineage::default());
        }
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(StoreError::InvalidRevisionLineage(
                "lineage metadata must be a regular file".into(),
            ));
        }
        let bytes = fs::read(path)?;
        let lineage: RevisionLineage = serde_json::from_slice(&bytes)
            .map_err(|error| StoreError::InvalidRevisionLineage(error.to_string()))?;
        if lineage.schema != REVISION_LINEAGE_SCHEMA {
            return Err(StoreError::InvalidRevisionLineage(format!(
                "unsupported lineage schema {}",
                lineage.schema
            )));
        }
        Ok(lineage)
    }

    fn write_lineage(&self, lineage: &RevisionLineage) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec_pretty(lineage)
            .map_err(|error| StoreError::InvalidRevisionLineage(error.to_string()))?;
        crate::atomic_write(&self.lineage_path(), &bytes)?;
        Ok(())
    }

    fn record_revision(&self, id: ProfileId, revision: u64) -> Result<(), StoreError> {
        let mut lineage = self.read_lineage()?;
        let key = id.to_string();
        if let Some(floor) = lineage.revisions.get(&key).copied() {
            if revision < floor {
                return Err(StoreError::RevisionRegression {
                    id,
                    revision,
                    floor,
                });
            }
            if revision == floor {
                return Ok(());
            }
        }
        lineage.revisions.insert(key, revision);
        self.write_lineage(&lineage)
    }

    fn ensure_tracked_revision(&self, profile: &Profile) -> Result<(), StoreError> {
        let lineage = self.read_lineage()?;
        let mut floor = lineage.revisions.get(&profile.id.to_string()).copied();
        let trash_path = self.trash_directory(profile.id).join(PROFILE_FILE);
        if trash_path.is_file() {
            let trashed = crate::load_profile(&trash_path)?;
            if trashed.id == profile.id {
                floor = Some(floor.map_or(trashed.revision, |known| known.max(trashed.revision)));
            }
        }
        if let Some(floor) = floor {
            if profile.revision < floor {
                return Err(StoreError::RevisionRegression {
                    id: profile.id,
                    revision: profile.revision,
                    floor,
                });
            }
        }
        Ok(())
    }

    fn import_revision_floor(&self, id: ProfileId) -> Result<Option<u64>, StoreError> {
        let lineage = self.read_lineage()?;
        let key = id.to_string();
        let mut floor = lineage.revisions.get(&key).copied();
        for path in self.revision_document_paths()? {
            let profile = crate::load_profile(&path)?;
            if profile.id == id {
                floor = Some(floor.map_or(profile.revision, |known| known.max(profile.revision)));
            }
        }
        Ok(floor)
    }

    fn revision_document_paths(&self) -> Result<Vec<PathBuf>, StoreError> {
        let mut paths = Vec::new();
        for root in [&self.profiles, &self.trash] {
            if !root.is_dir() {
                continue;
            }
            for entry in fs::read_dir(root)? {
                let entry = entry?;
                let file_type = entry.file_type()?;
                if !file_type.is_dir() || file_type.is_symlink() {
                    continue;
                }
                let name = entry.file_name();
                if root == &self.profiles && name.to_string_lossy().starts_with('.') {
                    continue;
                }
                let path = entry.path().join(PROFILE_FILE);
                if path.is_file() {
                    paths.push(path);
                }
            }
        }
        Ok(paths)
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

fn remap_scene_elements(
    references: &mut Vec<ElementId>,
    element_ids: &HashMap<ElementId, ElementId>,
) {
    *references = references
        .iter()
        .filter_map(|id| element_ids.get(id).copied())
        .collect();
}

fn remove_element_references(references: &mut Vec<ElementId>, removed: &HashSet<ElementId>) {
    references.retain(|id| !removed.contains(id));
}

fn remap_recognition(
    expression: &mut RecognitionExpression,
    element_ids: &HashMap<ElementId, ElementId>,
) {
    let conditions = match expression {
        RecognitionExpression::All { conditions } | RecognitionExpression::Any { conditions } => {
            conditions
        }
    };
    for condition in conditions {
        if let Some(id) = element_ids.get(&condition.element_id) {
            condition.element_id = *id;
        }
    }
}

fn remove_recognition_references(
    expression: &mut RecognitionExpression,
    removed: &HashSet<ElementId>,
) -> bool {
    let conditions = match expression {
        RecognitionExpression::All { conditions } | RecognitionExpression::Any { conditions } => {
            conditions
        }
    };
    let previous_count = conditions.len();
    conditions.retain(|condition| !removed.contains(&condition.element_id));
    conditions.len() != previous_count
}

fn remap_interaction_targets(
    targets: &mut [InteractionTarget],
    element_ids: &HashMap<ElementId, ElementId>,
) {
    for target in targets {
        target.id = crate::InteractionTargetId::new();
        if let Some(visibility) = &mut target.visibility {
            remap_recognition(visibility, element_ids);
        }
    }
}

fn remove_target_visibility_references(
    targets: &mut [InteractionTarget],
    removed: &HashSet<ElementId>,
) {
    for target in targets {
        if let Some(visibility) = &mut target.visibility {
            if remove_recognition_references(visibility, removed) {
                *visibility = RecognitionExpression::Any {
                    conditions: Vec::new(),
                };
            }
        }
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

fn rule_references_any(rule: &crate::EventRule, removed: &HashSet<ElementId>) -> bool {
    match &rule.predicate {
        RulePredicate::All { conditions } | RulePredicate::Any { conditions } => conditions
            .iter()
            .any(|condition| removed.contains(&condition.element_id)),
        RulePredicate::NumericBelow
        | RulePredicate::Boolean { .. }
        | RulePredicate::TextEquals { .. }
        | RulePredicate::TextContains { .. }
        | RulePredicate::RapidIncrease { .. } => false,
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
    #[error(transparent)]
    Archive(#[from] crate::ArchiveError),
    #[error("profile I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("profile {0} already exists")]
    AlreadyExists(ProfileId),
    #[error("stale profile revision: expected {expected}, current {current}")]
    RevisionConflict { expected: u64, current: u64 },
    #[error("profile {id} revision {revision} is below its recorded lineage floor {floor}")]
    RevisionRegression {
        id: ProfileId,
        revision: u64,
        floor: u64,
    },
    #[error("profile {id} revision cannot advance beyond u64::MAX")]
    RevisionExhausted { id: ProfileId },
    #[error("profile identity mismatch: expected {expected}, found {actual}")]
    ProfileIdentityMismatch {
        expected: ProfileId,
        actual: ProfileId,
    },
    #[error("revision lineage metadata is invalid: {0}")]
    InvalidRevisionLineage(String),
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
        AtomicRulePredicate, BarDirection, DerivedInput, DerivedObservation, Detector, DetectorId,
        Element, EventRule, InteractionPoint, InteractionRole, InteractionTarget,
        InteractionTargetId, NormalizedRegion, ObservationCondition, Overlay, OverlayId,
        RecognitionCondition, RecognitionExpression, RulePredicate, Scene, SceneId,
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
    fn revisions_cross_100_without_wrapping() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(directory.path(), 20);
        let mut profile = populated_profile();
        profile.revision = 99;
        store.create(&profile).unwrap();

        let hundred = store.commit(profile, 99).unwrap();
        assert_eq!(hundred.revision, 100);
        let one_hundred_one = store.commit(hundred, 100).unwrap();
        assert_eq!(one_hundred_one.revision, 101);
        assert!(store
            .profile_directory(one_hundred_one.id)
            .join("revisions/100.json")
            .is_file());
    }

    #[test]
    fn import_rebases_after_trash_and_keeps_the_old_lineage_recoverable() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(directory.path(), 20);
        let profile = populated_profile();
        let id = profile.id;
        store.create(&profile).unwrap();

        let mut high_revision = profile.clone();
        high_revision.name = "High revision".into();
        let high_revision = store.commit(high_revision, 0).unwrap();
        store.trash(id).unwrap();

        let mut imported_source = profile;
        imported_source.name = "Imported replacement".into();
        let source = directory.path().join("source");
        fs::create_dir_all(&source).unwrap();
        crate::save_profile(&source.join(PROFILE_FILE), &imported_source).unwrap();
        let archive = directory.path().join("replacement.hudprofile");
        crate::export_profile(&source, &archive).unwrap();

        let imported = store
            .import_archive(&archive, ImportLimits::default())
            .unwrap();
        assert_eq!(imported.id, id);
        assert_eq!(imported.revision, high_revision.revision + 1);
        assert_eq!(store.load(id).unwrap().name, "Imported replacement");
        assert_eq!(
            crate::load_profile(
                &directory
                    .path()
                    .join("trash")
                    .join(id.to_string())
                    .join(PROFILE_FILE)
            )
            .unwrap()
            .revision,
            high_revision.revision
        );
        assert!(!store
            .profiles_root()
            .join(".import-lineage-leftover.tmp")
            .exists());
    }

    #[test]
    fn lineage_survives_permanent_trash_deletion() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(directory.path(), 20);
        let profile = populated_profile();
        let id = profile.id;
        store.create(&profile).unwrap();
        let mut high_revision = profile.clone();
        high_revision.name = "High revision".into();
        let high_revision = store.commit(high_revision, 0).unwrap();
        store.trash(id).unwrap();
        store.permanently_delete_trashed(id).unwrap();
        drop(store);
        let store = ProfileStore::new(directory.path(), 20);

        let source = directory.path().join("source");
        fs::create_dir_all(&source).unwrap();
        let mut imported_source = high_revision.clone();
        imported_source.name = "Imported after deletion".into();
        imported_source.revision = 0;
        crate::save_profile(&source.join(PROFILE_FILE), &imported_source).unwrap();
        let archive = directory.path().join("replacement.hudprofile");
        crate::export_profile(&source, &archive).unwrap();

        let imported = store
            .import_archive(&archive, ImportLimits::default())
            .unwrap();
        assert_eq!(imported.revision, high_revision.revision + 1);
    }

    #[test]
    fn import_accounts_for_external_backup_directories() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(directory.path(), 20);
        let profile = populated_profile();
        let id = profile.id;
        let backup = store.profiles_root().join("backup-before-profile-import");
        fs::create_dir_all(&backup).unwrap();
        let mut backed_up = profile.clone();
        backed_up.revision = 7;
        crate::save_profile(&backup.join(PROFILE_FILE), &backed_up).unwrap();

        let source = directory.path().join("source");
        fs::create_dir_all(&source).unwrap();
        crate::save_profile(&source.join(PROFILE_FILE), &profile).unwrap();
        let archive = directory.path().join("replacement.hudprofile");
        crate::export_profile(&source, &archive).unwrap();

        let imported = store
            .import_archive(&archive, ImportLimits::default())
            .unwrap();
        assert_eq!(imported.id, id);
        assert_eq!(imported.revision, 8);
        assert_eq!(store.list().unwrap(), vec![imported]);
        assert_eq!(
            crate::load_profile(&backup.join(PROFILE_FILE)).unwrap(),
            backed_up
        );
    }

    #[test]
    fn load_and_list_reject_a_mismatched_profile_identity() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(directory.path(), 20);
        let profile = populated_profile();
        let id = profile.id;
        store.create(&profile).unwrap();
        let mut replaced = profile;
        replaced.id = ProfileId::new();
        crate::save_profile(&store.profile_directory(id).join(PROFILE_FILE), &replaced).unwrap();
        for result in [store.load(id).map(|profile| vec![profile]), store.list()] {
            assert!(
                matches!(result, Err(StoreError::ProfileIdentityMismatch { expected, actual })
                if expected == id && actual == replaced.id)
            );
        }
    }

    #[test]
    fn load_rejects_a_manually_regressed_active_document() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(directory.path(), 20);
        let profile = populated_profile();
        let id = profile.id;
        store.create(&profile).unwrap();
        let committed = store.commit(profile, 0).unwrap();

        let mut regressed = committed;
        regressed.revision = 0;
        crate::save_profile(&store.profile_directory(id).join(PROFILE_FILE), &regressed).unwrap();
        assert!(matches!(
            store.load(id),
            Err(StoreError::RevisionRegression {
                id: error_id,
                revision: 0,
                floor: 1,
            }) if error_id == id
        ));
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
    #[allow(clippy::too_many_lines)]
    fn deep_duplicate_rekeys_objects_copies_assets_and_resets_history() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(directory.path(), 20);
        let mut profile = populated_profile();
        profile
            .element_aliases
            .insert("legacy_signal".into(), profile.elements[0].id);
        profile.rules[0].predicate = RulePredicate::All {
            conditions: vec![ObservationCondition {
                element_id: profile.elements[0].id,
                predicate: AtomicRulePredicate::Boolean { expected: true },
            }],
        };
        let first_scene = SceneId::new();
        let second_scene = SceneId::new();
        let recognition = RecognitionExpression::All {
            conditions: vec![RecognitionCondition {
                element_id: profile.elements[0].id,
                predicate: AtomicRulePredicate::Boolean { expected: true },
                weight: 1.0,
            }],
        };
        let target = InteractionTarget {
            id: InteractionTargetId::new(),
            name: "confirm".into(),
            role: InteractionRole::Action,
            region: NormalizedRegion {
                x: 0.5,
                y: 0.5,
                width: 0.2,
                height: 0.1,
            },
            interaction_point: Some(InteractionPoint::Center),
            visibility: Some(recognition.clone()),
            required_for_json: true,
            caution_class: None,
            caution: None,
        };
        profile.scenes = vec![
            Scene {
                id: first_scene,
                name: "menu".into(),
                description: String::new(),
                enabled: true,
                priority: 0,
                recognition: recognition.clone(),
                minimum_confidence: 0.8,
                ambiguity_margin: 0.1,
                required_samples: 1,
                sample_window: 1,
                element_ids: vec![profile.elements[0].id],
                required_element_ids: vec![profile.elements[0].id],
                interaction_targets: vec![target.clone()],
                transition_hints: vec![second_scene],
            },
            Scene {
                id: second_scene,
                name: "gameplay".into(),
                description: String::new(),
                enabled: true,
                priority: 0,
                recognition: recognition.clone(),
                minimum_confidence: 0.8,
                ambiguity_margin: 0.1,
                required_samples: 1,
                sample_window: 1,
                element_ids: vec![profile.elements[0].id],
                required_element_ids: Vec::new(),
                interaction_targets: Vec::new(),
                transition_hints: vec![first_scene],
            },
        ];
        let mut overlay_target = target;
        overlay_target.id = InteractionTargetId::new();
        profile.overlays.push(Overlay {
            id: OverlayId::new(),
            name: "dialog".into(),
            description: String::new(),
            enabled: true,
            blocks_scene_targets: true,
            priority: 0,
            recognition,
            minimum_confidence: 0.8,
            required_samples: 1,
            sample_window: 1,
            element_ids: vec![profile.elements[0].id],
            required_element_ids: vec![profile.elements[0].id],
            interaction_targets: vec![overlay_target],
        });
        store.create(&profile).unwrap();
        let source = store.profile_directory(profile.id);
        fs::create_dir_all(source.join("templates")).unwrap();
        fs::write(source.join("templates/bar.bin"), b"asset").unwrap();
        let duplicate = store.duplicate_profile(profile.id, "Copy").unwrap();
        assert_ne!(duplicate.id, profile.id);
        assert_ne!(duplicate.elements[0].id, profile.elements[0].id);
        assert_eq!(
            duplicate.element_aliases.get("legacy_signal"),
            Some(&duplicate.elements[0].id)
        );
        assert_eq!(duplicate.rules[0].element_id, duplicate.elements[0].id);
        let RulePredicate::All { conditions } = &duplicate.rules[0].predicate else {
            panic!("composition was not preserved");
        };
        assert_eq!(conditions[0].element_id, duplicate.elements[0].id);
        assert_ne!(duplicate.scenes[0].id, first_scene);
        assert_ne!(duplicate.scenes[1].id, second_scene);
        assert_eq!(
            duplicate.scenes[0].transition_hints,
            vec![duplicate.scenes[1].id]
        );
        assert_eq!(
            duplicate.scenes[1].transition_hints,
            vec![duplicate.scenes[0].id]
        );
        assert_eq!(
            duplicate.scenes[0].element_ids,
            vec![duplicate.elements[0].id]
        );
        assert_eq!(
            duplicate.scenes[0].required_element_ids,
            vec![duplicate.elements[0].id]
        );
        assert_ne!(
            duplicate.scenes[0].interaction_targets[0].id,
            profile.scenes[0].interaction_targets[0].id
        );
        let RecognitionExpression::All { conditions } = &duplicate.scenes[0].interaction_targets[0]
            .visibility
            .as_ref()
            .unwrap()
        else {
            panic!("visibility expression was not preserved");
        };
        assert_eq!(conditions[0].element_id, duplicate.elements[0].id);
        assert_ne!(duplicate.overlays[0].id, profile.overlays[0].id);
        assert_eq!(
            duplicate.overlays[0].required_element_ids,
            vec![duplicate.elements[0].id]
        );
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
    fn deep_duplicate_rekeys_derived_detector_ids() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(directory.path(), 20);
        let mut profile = populated_profile();
        let derived_detector_id = DetectorId::new();
        profile.derived_observations.push(DerivedObservation {
            id: ElementId::new(),
            detector_id: derived_detector_id,
            name: "stage_label".into(),
            enabled: true,
            format: "{Health}".into(),
            inputs: vec![DerivedInput {
                name: "Health".into(),
                element_id: profile.elements[0].id,
            }],
        });
        store.create(&profile).unwrap();

        let duplicate = store.duplicate_profile(profile.id, "Copy").unwrap();

        assert_ne!(
            duplicate.derived_observations[0].detector_id,
            derived_detector_id
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

    #[test]
    #[allow(clippy::too_many_lines)]
    fn removing_element_prunes_dependent_profile_references() {
        let mut profile = populated_profile();
        let raw_id = profile.elements[0].id;
        let derived_id = ElementId::new();
        let chained_id = ElementId::new();
        profile.derived_observations = vec![
            DerivedObservation {
                id: derived_id,
                detector_id: DetectorId::new(),
                name: "stage".into(),
                enabled: true,
                format: "{health}".into(),
                inputs: vec![DerivedInput {
                    name: "health".into(),
                    element_id: raw_id,
                }],
            },
            DerivedObservation {
                id: chained_id,
                detector_id: DetectorId::new(),
                name: "summary".into(),
                enabled: true,
                format: "{stage}".into(),
                inputs: vec![DerivedInput {
                    name: "stage".into(),
                    element_id: derived_id,
                }],
            },
        ];
        profile.global_elements = vec![raw_id, chained_id];
        profile.rules[0].element_id = chained_id;
        profile.rules[0].predicate = RulePredicate::All {
            conditions: vec![ObservationCondition {
                element_id: raw_id,
                predicate: AtomicRulePredicate::Boolean { expected: true },
            }],
        };

        let recognition = RecognitionExpression::All {
            conditions: vec![RecognitionCondition {
                element_id: raw_id,
                predicate: AtomicRulePredicate::Boolean { expected: true },
                weight: 1.0,
            }],
        };
        let target = InteractionTarget {
            id: InteractionTargetId::new(),
            name: "confirm".into(),
            role: InteractionRole::Action,
            region: NormalizedRegion {
                x: 0.4,
                y: 0.4,
                width: 0.2,
                height: 0.1,
            },
            interaction_point: Some(InteractionPoint::Center),
            visibility: Some(recognition.clone()),
            required_for_json: true,
            caution_class: None,
            caution: None,
        };
        profile.scenes.push(Scene {
            id: SceneId::new(),
            name: "menu".into(),
            description: String::new(),
            enabled: true,
            priority: 0,
            recognition: recognition.clone(),
            minimum_confidence: 0.8,
            ambiguity_margin: 0.1,
            required_samples: 1,
            sample_window: 1,
            element_ids: vec![raw_id],
            required_element_ids: vec![raw_id],
            interaction_targets: vec![target.clone()],
            transition_hints: Vec::new(),
        });
        let mut overlay_target = target;
        overlay_target.id = InteractionTargetId::new();
        profile.overlays.push(Overlay {
            id: OverlayId::new(),
            name: "dialog".into(),
            description: String::new(),
            enabled: true,
            blocks_scene_targets: true,
            priority: 0,
            recognition,
            minimum_confidence: 0.8,
            required_samples: 1,
            sample_window: 1,
            element_ids: vec![raw_id],
            required_element_ids: vec![raw_id],
            interaction_targets: vec![overlay_target],
        });
        profile.validate().unwrap();
        profile.element_aliases.insert("raw_alias".into(), raw_id);

        ProfileStore::remove_element(&mut profile, raw_id).unwrap();

        assert!(profile.elements.is_empty());
        assert!(profile.derived_observations.is_empty());
        assert!(profile.element_aliases.is_empty());
        assert!(profile.global_elements.is_empty());
        assert!(profile.rules.is_empty());
        assert!(profile.scenes[0].element_ids.is_empty());
        assert!(profile.scenes[0].required_element_ids.is_empty());
        assert!(!profile.scenes[0].enabled);
        assert!(profile.scenes[0].interaction_targets[0]
            .visibility
            .as_ref()
            .is_some_and(|expression| matches!(expression, RecognitionExpression::Any { conditions } if conditions.is_empty())));
        assert!(profile.overlays[0].element_ids.is_empty());
        assert!(profile.overlays[0].required_element_ids.is_empty());
        assert!(!profile.overlays[0].enabled);
        assert!(profile.overlays[0].interaction_targets[0]
            .visibility
            .as_ref()
            .is_some_and(|expression| matches!(expression, RecognitionExpression::Any { conditions } if conditions.is_empty())));
        profile.validate().unwrap();
    }
}
