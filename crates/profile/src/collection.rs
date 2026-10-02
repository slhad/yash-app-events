//! Discovery and self-contained packaging of routing collections.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use uuid::Uuid;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

use crate::{
    ArchiveError, ImportLimits, Manifest, ManifestFile, Profile, ProfileBundle, ProfileBundleError,
    ProfileId, ProfileStore, StorageError, StoreError,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BundleSummary {
    pub id: ProfileId,
    pub name: String,
    pub game: String,
    pub profile_ids: Vec<ProfileId>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BundleDiscoveryIssue {
    pub source: String,
    pub detail: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct BundleDiscoveryPage {
    pub bundles: Vec<BundleSummary>,
    pub errors: Vec<BundleDiscoveryIssue>,
    pub next_after: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BundlePackageManifest {
    pub schema: u16,
    pub router_profile_id: ProfileId,
    pub files: Vec<ManifestFile>,
}

impl ProfileBundle {
    #[must_use]
    pub fn profile_ids(&self) -> Vec<ProfileId> {
        let mut seen = HashSet::new();
        std::iter::once(self.router_profile_id)
            .chain(self.members.iter().map(|m| m.profile_id))
            .filter(|id| seen.insert(*id))
            .collect()
    }
}

/// Checks every pinned document and route before packaging or admission.
///
/// # Errors
/// Returns an identity, revision, game, layout, or router-route mismatch.
pub fn validate_bundle_profiles(
    bundle: &ProfileBundle,
    profiles: &[Profile],
) -> Result<(), CollectionError> {
    bundle.validate()?;
    let by_id: HashMap<_, _> = profiles.iter().map(|p| (p.id, p)).collect();
    if by_id.len() != profiles.len() || by_id.len() != bundle.profile_ids().len() {
        return Err(CollectionError::Invalid(
            "profile inventory differs from routing manifest".into(),
        ));
    }
    let router = by_id
        .get(&bundle.router_profile_id)
        .ok_or_else(|| CollectionError::Invalid("router is missing".into()))?;
    let pins = std::iter::once((bundle.router_profile_id, bundle.router_profile_revision)).chain(
        bundle
            .members
            .iter()
            .map(|m| (m.profile_id, m.profile_revision)),
    );
    for (id, revision) in pins {
        let p = by_id
            .get(&id)
            .ok_or_else(|| CollectionError::Invalid(format!("profile {id} is missing")))?;
        p.validate().map_err(StorageError::from)?;
        if p.revision != revision
            || p.game != bundle.game
            || p.layout.reference_width != router.layout.reference_width
            || p.layout.reference_height != router.layout.reference_height
        {
            return Err(CollectionError::Invalid(format!(
                "profile {id} revision/game/layout differs from bundle pins"
            )));
        }
    }
    for member in &bundle.members {
        if member
            .scene_ids
            .iter()
            .any(|id| !router.scenes.iter().any(|s| s.id == *id))
            || member
                .overlay_ids
                .iter()
                .any(|id| !router.overlays.iter().any(|o| o.id == *id))
        {
            return Err(CollectionError::Invalid(format!(
                "member {} references an unknown router scene/overlay",
                member.profile_id
            )));
        }
    }
    Ok(())
}

impl ProfileStore {
    #[must_use]
    pub fn bundles_root(&self) -> PathBuf {
        self.profiles_root()
            .parent()
            .unwrap_or(self.profiles_root())
            .join("bundles")
    }

    fn managed_bundle_directories(&self) -> Result<Vec<PathBuf>, std::io::Error> {
        let root = self.bundles_root();
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut paths = Vec::new();
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && Uuid::parse_str(&entry.file_name().to_string_lossy())
                    .is_ok_and(|id| id.to_string() == entry.file_name().to_string_lossy())
            {
                paths.push(entry.path());
                if paths.len() > 128 {
                    return Err(std::io::Error::other("installed bundle count exceeds 128"));
                }
            }
        }
        paths.sort();
        Ok(paths)
    }

    pub(crate) fn installed_profile_directory(&self, id: ProfileId) -> Option<PathBuf> {
        self.managed_bundle_directories()
            .ok()?
            .into_iter()
            .map(|p| p.join("profiles").join(id.to_string()))
            .find(|p| {
                fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
                    && p.join("profile.json").is_file()
            })
    }

    pub(crate) fn installed_bundle_profile_ids(&self) -> Result<Vec<ProfileId>, StoreError> {
        let mut ids = Vec::new();
        for path in self.managed_bundle_directories()? {
            let profiles = path.join("profiles");
            if !profiles.is_dir() {
                continue;
            }
            for entry in fs::read_dir(profiles)? {
                let entry = entry?;
                let Ok(uuid) = Uuid::parse_str(&entry.file_name().to_string_lossy()) else {
                    continue;
                };
                if entry.file_type()?.is_dir()
                    && uuid.to_string() == entry.file_name().to_string_lossy()
                    && entry.path().join("profile.json").is_file()
                {
                    ids.push(
                        serde_json::from_value(serde_json::json!(uuid))
                            .expect("UUID profile identity"),
                    );
                }
            }
        }
        Ok(ids)
    }

    fn bundle_sources(&self) -> Result<Vec<(String, PathBuf)>, CollectionError> {
        let mut sources: Vec<_> = self
            .managed_bundle_directories()?
            .into_iter()
            .map(|p| {
                (
                    format!("installed:{}", p.file_name().unwrap().to_string_lossy()),
                    p.join("bundle.json"),
                )
            })
            .collect();
        if self.profiles_root().exists() {
            for entry in fs::read_dir(self.profiles_root())? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if entry.file_type()?.is_file() && name.ends_with(".profile-bundle.json") {
                    sources.push((format!("local:{name}"), entry.path()));
                    if sources.len() > 256 {
                        return Err(CollectionError::Invalid(
                            "bundle discovery exceeds 256 sources".into(),
                        ));
                    }
                }
            }
        }
        sources.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(sources)
    }

    /// Lists at most 16 routing manifests with independent reference diagnostics.
    ///
    /// # Errors
    /// Returns enumeration errors. Individual malformed manifests remain diagnostics.
    pub fn list_bundles(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> Result<BundleDiscoveryPage, CollectionError> {
        let sources: Vec<_> = self
            .bundle_sources()?
            .into_iter()
            .filter(|(key, _)| after.is_none_or(|a| key.as_str() > a))
            .collect();
        let limit = limit.clamp(1, 16);
        let mut page = BundleDiscoveryPage::default();
        for (source, path) in sources.iter().take(limit) {
            match crate::load_profile_bundle(path) {
                Ok(bundle) => {
                    let profile_ids = bundle.profile_ids();
                    let error = self
                        .bundle_profiles(&bundle)
                        .err()
                        .map(|e| e.to_string().chars().take(1024).collect());
                    page.bundles.push(BundleSummary {
                        id: bundle.router_profile_id,
                        name: bundle.name,
                        game: bundle.game.chars().take(128).collect(),
                        profile_ids,
                        error,
                    });
                }
                Err(error) => page.errors.push(BundleDiscoveryIssue {
                    source: source.chars().take(256).collect(),
                    detail: error.to_string().chars().take(1024).collect(),
                }),
            }
        }
        if sources.len() > limit {
            page.next_after = Some(sources[limit - 1].0.clone());
        }
        Ok(page)
    }

    /// Gets a discovered manifest by its stable router identity.
    ///
    /// # Errors
    /// Rejects ambiguous duplicate manifests and unavailable bundle identities.
    pub fn load_bundle(&self, id: ProfileId) -> Result<ProfileBundle, CollectionError> {
        let mut found = None;
        for (_, path) in self.bundle_sources()? {
            if let Ok(bundle) = crate::load_profile_bundle(&path) {
                if bundle.router_profile_id == id {
                    if found.is_some() {
                        return Err(CollectionError::Invalid(format!(
                            "multiple bundle manifests use router {id}"
                        )));
                    }
                    found = Some(bundle);
                }
            }
        }
        found.ok_or_else(|| CollectionError::Invalid(format!("bundle {id} is unavailable")))
    }

    fn bundle_profiles(&self, bundle: &ProfileBundle) -> Result<Vec<Profile>, CollectionError> {
        let profiles = bundle
            .profile_ids()
            .into_iter()
            .map(|id| self.load(id))
            .collect::<Result<Vec<_>, _>>()?;
        validate_bundle_profiles(bundle, &profiles)?;
        Ok(profiles)
    }

    /// Exports a discovered collection with every referenced portable profile.
    ///
    /// # Errors
    /// Rejects bad references, assets, or unsafe archive content.
    pub fn export_bundle(
        &self,
        id: ProfileId,
        destination: &Path,
    ) -> Result<BundlePackageManifest, CollectionError> {
        let bundle = self.load_bundle(id)?;
        self.bundle_profiles(&bundle)?;
        export_bundle(&bundle, |id| self.profile_directory(id), destination)
    }

    /// Stages every document, rebases lineage/pins, then publishes one collection.
    ///
    /// # Errors
    /// Rejects invalid archives, live identity collisions, resource and write failures.
    pub fn import_bundle(
        &self,
        archive: &Path,
        limits: ImportLimits,
    ) -> Result<ProfileBundle, CollectionError> {
        if self.managed_bundle_directories()?.len() >= 128 {
            return Err(CollectionError::Invalid(
                "installed bundle count exceeds 128".into(),
            ));
        }
        let root = self.bundles_root();
        fs::create_dir_all(&root)?;
        fs::create_dir_all(self.profiles_root())?;
        let staging = root.join(format!(".import-{}", Uuid::new_v4()));
        fs::create_dir(&staging)?;
        let result = (|| {
            let mut bundle = extract_bundle(archive, &staging, limits)?;
            let destination = root.join(bundle.router_profile_id.to_string());
            if destination.exists() {
                return Err(CollectionError::Invalid(
                    "bundle identity is already installed".into(),
                ));
            }
            for id in bundle.profile_ids() {
                if self.profile_directory(id).exists() {
                    return Err(StoreError::AlreadyExists(id).into());
                }
            }
            for id in bundle.profile_ids() {
                let path = staging
                    .join("profiles")
                    .join(id.to_string())
                    .join("profile.json");
                let mut profile = crate::load_profile(&path)?;
                if let Some(floor) = self.import_revision_floor(id)? {
                    profile.revision = profile.revision.max(
                        floor
                            .checked_add(1)
                            .ok_or(StoreError::RevisionExhausted { id })?,
                    );
                    crate::save_profile(&path, &profile)?;
                }
                if id == bundle.router_profile_id {
                    bundle.router_profile_revision = profile.revision;
                }
                for member in &mut bundle.members {
                    if member.profile_id == id {
                        member.profile_revision = profile.revision;
                    }
                }
                // Monotonic high-water marks may advance on a failed publication, never regress.
                self.record_revision(id, profile.revision)?;
            }
            crate::atomic_write(
                &staging.join("bundle.json"),
                &serde_json::to_vec_pretty(&bundle)?,
            )?;
            fs::rename(&staging, &destination)?;
            Ok(bundle)
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(staging);
        }
        result
    }
}

/// Builds a self-contained collection using the existing portable-profile exporter.
///
/// # Errors
/// Rejects invalid references/assets and archive resource-limit violations.
pub fn export_bundle(
    bundle: &ProfileBundle,
    directory: impl Fn(ProfileId) -> PathBuf,
    destination: &Path,
) -> Result<BundlePackageManifest, CollectionError> {
    let profiles = bundle
        .profile_ids()
        .into_iter()
        .map(|id| crate::load_profile(&directory(id).join("profile.json")))
        .collect::<Result<Vec<_>, _>>()?;
    validate_bundle_profiles(bundle, &profiles)?;
    let parent = destination
        .parent()
        .ok_or_else(|| CollectionError::Invalid("destination has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let staging = parent.join(format!(".bundle-export-{}", Uuid::new_v4()));
    fs::create_dir(&staging)?;
    let result = (|| {
        fs::write(
            staging.join("bundle.json"),
            serde_json::to_vec_pretty(bundle)?,
        )?;
        fs::create_dir(staging.join("profiles"))?;
        let mut expanded_bytes = fs::metadata(staging.join("bundle.json"))?.len();
        let mut expanded_files = 0_usize;
        let mut names = vec!["bundle.json".to_owned()];
        let limits = ImportLimits::for_bundle();
        for id in bundle.profile_ids() {
            let name = format!("profiles/{id}.hudprofile");
            let manifest = crate::export_profile(&directory(id), &staging.join(&name))?;
            let profile_bytes = manifest
                .files
                .iter()
                .try_fold(0_u64, |total, file| total.checked_add(file.size))
                .ok_or(ArchiveError::LimitExceeded("profile total size"))?;
            if profile_bytes > ImportLimits::default().maximum_total_bytes
                || manifest.files.len() > ImportLimits::default().maximum_files
            {
                return Err(ArchiveError::LimitExceeded("profile inventory").into());
            }
            for file in manifest.files {
                if file.size > limits.maximum_file_bytes {
                    return Err(ArchiveError::LimitExceeded("expanded individual file size").into());
                }
                expanded_bytes = expanded_bytes
                    .checked_add(file.size)
                    .ok_or(ArchiveError::LimitExceeded("total size"))?;
                expanded_files += 1;
            }
            if expanded_bytes > limits.maximum_total_bytes || expanded_files > limits.maximum_files
            {
                return Err(ArchiveError::LimitExceeded("bundle expanded inventory").into());
            }
            names.push(name);
        }
        names.sort();
        let mut files = Vec::new();
        let mut archive_bytes = 0_u64;
        for name in &names {
            let bytes = fs::read(staging.join(name))?;
            if bytes.len() as u64 > limits.maximum_file_bytes {
                return Err(ArchiveError::LimitExceeded("individual file size").into());
            }
            archive_bytes = archive_bytes
                .checked_add(bytes.len() as u64)
                .ok_or(ArchiveError::LimitExceeded("outer total size"))?;
            if archive_bytes > limits.maximum_total_bytes {
                return Err(ArchiveError::LimitExceeded("outer total size").into());
            }
            files.push(ManifestFile {
                path: name.clone(),
                size: bytes.len() as u64,
                sha256: format!("{:x}", Sha256::digest(&bytes)),
            });
        }
        let manifest = BundlePackageManifest {
            schema: 1,
            router_profile_id: bundle.router_profile_id,
            files,
        };
        let temporary = staging.join("package.tmp");
        let mut writer = ZipWriter::new(File::create(&temporary)?);
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(0o600);
        writer.start_file("manifest.json", options)?;
        writer.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
        for name in names {
            writer.start_file(&name, options)?;
            writer.write_all(&fs::read(staging.join(name))?)?;
        }
        writer.finish()?.sync_all()?;
        fs::rename(temporary, destination)?;
        Ok(manifest)
    })();
    let _ = fs::remove_dir_all(staging);
    result
}

fn extract_bundle(
    archive_path: &Path,
    staging: &Path,
    limits: ImportLimits,
) -> Result<ProfileBundle, CollectionError> {
    let mut archive = ZipArchive::new(File::open(archive_path)?)?;
    if archive.len() > 35 {
        return Err(ArchiveError::LimitExceeded("bundle file count").into());
    }
    let mut manifest_count = 0;
    for i in 0..archive.len() {
        let entry = archive.by_index(i)?;
        if entry.name() == "manifest.json" {
            manifest_count += 1;
            if entry.is_dir()
                || entry
                    .unix_mode()
                    .is_some_and(|m| m & 0o170_000 == 0o120_000)
                || entry.size() > 256 * 1024
            {
                return Err(ArchiveError::InvalidManifest.into());
            }
        }
    }
    if manifest_count != 1 {
        return Err(ArchiveError::InvalidManifest.into());
    }
    let manifest: BundlePackageManifest = read_zip_json(&mut archive, "manifest.json", 256 * 1024)?;
    if manifest.schema != 1 || manifest.files.len() > 34 {
        return Err(ArchiveError::InvalidManifest.into());
    }
    let extraction = staging.join("archives");
    fs::create_dir(&extraction)?;
    let portable = Manifest {
        schema: 1,
        profile_schema: crate::PROFILE_SCHEMA_VERSION,
        profile_id: manifest.router_profile_id,
        files: manifest.files.clone(),
    };
    crate::archive::extract_archive(&mut archive, &portable, &extraction, limits)?;
    let bundle = crate::load_profile_bundle(&extraction.join("bundle.json"))?;
    let expected: HashSet<_> = std::iter::once("bundle.json".to_owned())
        .chain(
            bundle
                .profile_ids()
                .into_iter()
                .map(|id| format!("profiles/{id}.hudprofile")),
        )
        .collect();
    let actual: HashSet<_> = manifest.files.iter().map(|f| f.path.clone()).collect();
    if bundle.router_profile_id != manifest.router_profile_id
        || actual != expected
        || actual.len() != manifest.files.len()
    {
        return Err(ArchiveError::InvalidManifest.into());
    }
    let mut remaining = limits;
    remaining.maximum_total_bytes = remaining
        .maximum_total_bytes
        .checked_sub(fs::metadata(extraction.join("bundle.json"))?.len())
        .ok_or(ArchiveError::LimitExceeded("bundle total size"))?;
    let mut profiles = Vec::new();
    for id in bundle.profile_ids() {
        let path = extraction.join(format!("profiles/{id}.hudprofile"));
        let mut nested = ZipArchive::new(File::open(&path)?)?;
        if nested.len() > remaining.maximum_files.saturating_add(1) {
            return Err(ArchiveError::LimitExceeded("nested file count").into());
        }
        let inventory: Manifest = read_zip_json(
            &mut nested,
            "manifest.json",
            remaining.maximum_file_bytes.min(4 * 1024 * 1024),
        )?;
        let bytes = inventory
            .files
            .iter()
            .try_fold(0_u64, |total, f| total.checked_add(f.size))
            .ok_or(ArchiveError::LimitExceeded("bundle total size"))?;
        if bytes > remaining.maximum_total_bytes || inventory.files.len() > remaining.maximum_files
        {
            return Err(ArchiveError::LimitExceeded("bundle expanded inventory").into());
        }
        let profile_limits = ImportLimits {
            maximum_total_bytes: remaining
                .maximum_total_bytes
                .min(ImportLimits::default().maximum_total_bytes),
            ..remaining
        };
        let profile = crate::import_profile(&path, &staging.join("profiles"), profile_limits)?;
        if profile.id != id {
            return Err(ArchiveError::InvalidManifest.into());
        }
        remaining.maximum_total_bytes -= bytes;
        remaining.maximum_files -= inventory.files.len();
        profiles.push(profile);
    }
    validate_bundle_profiles(&bundle, &profiles)?;
    fs::write(
        staging.join("bundle.json"),
        serde_json::to_vec_pretty(&bundle)?,
    )?;
    fs::remove_dir_all(extraction)?;
    Ok(bundle)
}

fn read_zip_json<T: serde::de::DeserializeOwned>(
    archive: &mut ZipArchive<File>,
    name: &str,
    maximum: u64,
) -> Result<T, CollectionError> {
    let entry = archive.by_name(name)?;
    if entry.size() > maximum
        || entry.is_dir()
        || entry
            .unix_mode()
            .is_some_and(|m| m & 0o170_000 == 0o120_000)
    {
        return Err(ArchiveError::InvalidManifest.into());
    }
    let mut bytes = Vec::new();
    entry
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err(ArchiveError::LimitExceeded("manifest size").into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}

#[derive(Debug, Error)]
pub enum CollectionError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Bundle(#[from] ProfileBundleError),
    #[error("invalid routing collection: {0}")]
    Invalid(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProfileBundleMember, PROFILE_BUNDLE_SCHEMA_VERSION};

    fn fixture(root: &Path) -> (ProfileStore, ProfileBundle) {
        let store = ProfileStore::new(root, 20);
        let router = Profile::new("Queen router", "queen", 558, 992);
        let member = Profile::new("Battle", "queen", 558, 992);
        store.create(&router).unwrap();
        store.create(&member).unwrap();
        let bundle = ProfileBundle {
            schema: PROFILE_BUNDLE_SCHEMA_VERSION,
            name: "Queen Blade".into(),
            game: "queen".into(),
            router_profile_id: router.id,
            router_profile_revision: 0,
            members: vec![ProfileBundleMember {
                profile_id: member.id,
                profile_revision: 0,
                scene_ids: vec![],
                overlay_ids: vec![],
                priority: 0,
                fallback: true,
            }],
        };
        crate::atomic_write(
            &store.profiles_root().join("queen.profile-bundle.json"),
            &serde_json::to_vec(&bundle).unwrap(),
        )
        .unwrap();
        (store, bundle)
    }

    #[test]
    fn collection_round_trip_is_one_atomic_install_and_remains_editable() {
        let temp = tempfile::tempdir().unwrap();
        let (source, bundle) = fixture(&temp.path().join("source"));
        let archive = temp.path().join("queen.hudbundle");
        source
            .export_bundle(bundle.router_profile_id, &archive)
            .unwrap();
        let destination = ProfileStore::new(temp.path().join("destination"), 20);
        let installed = destination
            .import_bundle(&archive, ImportLimits::default())
            .unwrap();
        assert_eq!(installed, bundle);
        assert_eq!(destination.list().unwrap().len(), 2);
        assert_eq!(destination.list_bundles(None, 16).unwrap().bundles.len(), 1);
        assert_eq!(fs::read_dir(destination.bundles_root()).unwrap().count(), 1);
        assert!(destination
            .import_bundle(&archive, ImportLimits::default())
            .is_err());
        let profile = destination.load(bundle.members[0].profile_id).unwrap();
        assert_eq!(destination.commit(profile, 0).unwrap().revision, 1);
        assert!(destination.list_bundles(None, 16).unwrap().bundles[0]
            .error
            .as_ref()
            .unwrap()
            .contains("pins"));
        assert_eq!(
            destination
                .list_revisions(bundle.members[0].profile_id)
                .unwrap()
                .len(),
            2
        );
        destination.trash(bundle.members[0].profile_id).unwrap();
        assert!(destination.load(bundle.members[0].profile_id).is_err());
        destination.restore(bundle.members[0].profile_id).unwrap();
        assert_eq!(
            destination
                .load(bundle.members[0].profile_id)
                .unwrap()
                .revision,
            1
        );
    }

    #[test]
    fn discovery_is_paged_and_keeps_bad_manifests_and_stale_pins_visible() {
        let temp = tempfile::tempdir().unwrap();
        let (store, bundle) = fixture(temp.path());
        let path = store.profiles_root().join("aaa.profile-bundle.json");
        fs::write(&path, "bad JSON").unwrap();
        let first = store.list_bundles(None, 1).unwrap();
        assert_eq!(first.errors.len(), 1);
        let second = store.list_bundles(first.next_after.as_deref(), 1).unwrap();
        assert_eq!(second.bundles[0].id, bundle.router_profile_id);
        assert!(second.bundles[0].error.is_none());
        assert!(second.next_after.is_none());
        let mut member = store.load(bundle.members[0].profile_id).unwrap();
        member.revision = 7;
        crate::save_profile(
            &store.profile_directory(member.id).join("profile.json"),
            &member,
        )
        .unwrap();
        assert!(store.list_bundles(None, 16).unwrap().bundles[0]
            .error
            .is_some());
        assert!(store
            .export_bundle(
                bundle.router_profile_id,
                &temp.path().join("stale.hudbundle")
            )
            .is_err());
        assert!(!temp.path().join("stale.hudbundle").exists());
    }

    #[test]
    fn collision_and_aggregate_limits_leave_no_partial_collection() {
        let temp = tempfile::tempdir().unwrap();
        let (source, bundle) = fixture(&temp.path().join("source"));
        let archive = temp.path().join("queen.hudbundle");
        source
            .export_bundle(bundle.router_profile_id, &archive)
            .unwrap();
        let destination = ProfileStore::new(temp.path().join("destination"), 20);
        let existing = source.load(bundle.members[0].profile_id).unwrap();
        destination.create(&existing).unwrap();
        assert!(destination
            .import_bundle(&archive, ImportLimits::default())
            .is_err());
        assert_eq!(destination.list().unwrap(), vec![existing]);
        assert_eq!(fs::read_dir(destination.bundles_root()).unwrap().count(), 0);
        let clean = ProfileStore::new(temp.path().join("clean"), 20);
        let limits = ImportLimits {
            maximum_files: 1,
            ..ImportLimits::default()
        };
        assert!(clean.import_bundle(&archive, limits).is_err());
        assert!(clean.list().unwrap().is_empty());
        assert_eq!(fs::read_dir(clean.bundles_root()).unwrap().count(), 0);
    }

    #[test]
    fn retained_lineage_rebases_every_pin_without_overwriting_trashed_data() {
        let temp = tempfile::tempdir().unwrap();
        let (source, bundle) = fixture(&temp.path().join("source"));
        let archive = temp.path().join("queen.hudbundle");
        source
            .export_bundle(bundle.router_profile_id, &archive)
            .unwrap();
        let destination = ProfileStore::new(temp.path().join("destination"), 20);
        for id in bundle.profile_ids() {
            let p = source.load(id).unwrap();
            destination.create(&p).unwrap();
            let p = destination.commit(p, 0).unwrap();
            destination.commit(p, 1).unwrap();
            destination.trash(id).unwrap();
        }
        let installed = destination
            .import_bundle(&archive, ImportLimits::default())
            .unwrap();
        assert_eq!(installed.router_profile_revision, 3);
        assert_eq!(installed.members[0].profile_revision, 3);
        for id in bundle.profile_ids() {
            assert_eq!(destination.load(id).unwrap().revision, 3);
        }
        assert!(destination.list_bundles(None, 16).unwrap().bundles[0]
            .error
            .is_none());
    }

    #[test]
    fn tampered_outer_archive_and_unsafe_paths_are_rejected_before_publication() {
        let temp = tempfile::tempdir().unwrap();
        let (source, bundle) = fixture(&temp.path().join("source"));
        let archive = temp.path().join("queen.hudbundle");
        source
            .export_bundle(bundle.router_profile_id, &archive)
            .unwrap();
        for (label, extra) in [
            ("tampered", "bundle.json"),
            ("traversal", "../escape"),
            ("undeclared", "extra.json"),
        ] {
            let mut reader = ZipArchive::new(File::open(&archive).unwrap()).unwrap();
            let path = temp.path().join(format!("{label}.hudbundle"));
            let mut writer = ZipWriter::new(File::create(&path).unwrap());
            let options = SimpleFileOptions::default();
            for i in 0..reader.len() {
                let mut entry = reader.by_index(i).unwrap();
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                if label == "tampered" && entry.name() == extra {
                    bytes.push(b' ');
                }
                writer.start_file(entry.name(), options).unwrap();
                writer.write_all(&bytes).unwrap();
            }
            if label != "tampered" {
                writer.start_file(extra, options).unwrap();
                writer.write_all(b"{}").unwrap();
            }
            writer.finish().unwrap();
            let store = ProfileStore::new(temp.path().join(label), 20);
            assert!(store.import_bundle(&path, ImportLimits::default()).is_err());
            assert!(store.list().unwrap().is_empty());
            assert_eq!(fs::read_dir(store.bundles_root()).unwrap().count(), 0);
            assert!(!temp.path().join("escape").exists());
        }
    }
    #[test]
    fn reference_validation_rejects_missing_identity_revision_game_layout_and_routes() {
        let temp = tempfile::tempdir().unwrap();
        let (store, bundle) = fixture(temp.path());
        let valid = bundle
            .profile_ids()
            .into_iter()
            .map(|id| store.load(id).unwrap())
            .collect::<Vec<_>>();
        validate_bundle_profiles(&bundle, &valid).unwrap();
        for field in ["identity", "revision", "game", "layout", "missing"] {
            let mut profiles = valid.clone();
            match field {
                "identity" => profiles[1].id = ProfileId::new(),
                "revision" => profiles[1].revision += 1,
                "game" => profiles[1].game = "other".into(),
                "layout" => profiles[1].layout.reference_width += 1,
                _ => {
                    profiles.pop();
                }
            }
            assert!(
                validate_bundle_profiles(&bundle, &profiles).is_err(),
                "{field}"
            );
        }
        let mut routed = bundle.clone();
        routed.members[0].fallback = false;
        routed.members[0].scene_ids = vec![crate::SceneId::new()];
        assert!(validate_bundle_profiles(&routed, &valid).is_err());
        routed.members[0].scene_ids.clear();
        routed.members[0].overlay_ids = vec![crate::OverlayId::new()];
        assert!(validate_bundle_profiles(&routed, &valid).is_err());
        routed.members[0].profile_id = routed.router_profile_id;
        routed.members.push(routed.members[0].clone());
        assert!(routed.validate().is_err());
    }

    #[test]
    fn missing_assets_and_symlinks_do_not_produce_a_collection_archive() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let (store, bundle) = fixture(temp.path());
        let mut member = store.load(bundle.members[0].profile_id).unwrap();
        member.elements.push(crate::Element {
            id: crate::ElementId::new(),
            name: "template".into(),
            enabled: true,
            color: "#ffffff".into(),
            region: crate::NormalizedRegion {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            detector: crate::Detector::Template {
                id: crate::DetectorId::new(),
                templates: vec!["templates/test.json".into()],
                masks: vec![],
                threshold: 0.9,
                preprocessing: vec![],
            },
        });
        let root = store.profile_directory(member.id);
        crate::save_profile(&root.join("profile.json"), &member).unwrap();
        let destination = temp.path().join("bad.hudbundle");
        assert!(store
            .export_bundle(bundle.router_profile_id, &destination)
            .is_err());
        fs::create_dir(root.join("templates")).unwrap();
        fs::write(temp.path().join("pixels.json"), b"{}").unwrap();
        symlink(
            temp.path().join("pixels.json"),
            root.join("templates/test.json"),
        )
        .unwrap();
        assert!(store
            .export_bundle(bundle.router_profile_id, &destination)
            .is_err());
        assert!(!destination.exists());
    }

    #[test]
    fn links_in_outer_and_nested_manifests_are_rejected_even_with_matching_hashes() {
        let temp = tempfile::tempdir().unwrap();
        let (source, bundle) = fixture(&temp.path().join("source"));
        let archive = temp.path().join("queen.hudbundle");
        source
            .export_bundle(bundle.router_profile_id, &archive)
            .unwrap();
        for nested_link in [false, true] {
            let mut reader = ZipArchive::new(File::open(&archive).unwrap()).unwrap();
            let mut entries = Vec::new();
            for i in 0..reader.len() {
                let mut e = reader.by_index(i).unwrap();
                let mut bytes = Vec::new();
                e.read_to_end(&mut bytes).unwrap();
                entries.push((e.name().to_owned(), bytes));
            }
            if nested_link {
                let nested = entries
                    .iter_mut()
                    .find(|(n, _)| n.ends_with(".hudprofile"))
                    .unwrap();
                let mut old = ZipArchive::new(std::io::Cursor::new(&nested.1)).unwrap();
                let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
                for i in 0..old.len() {
                    let mut e = old.by_index(i).unwrap();
                    let mut bytes = Vec::new();
                    e.read_to_end(&mut bytes).unwrap();
                    if e.name() == "manifest.json" {
                        writer
                            .add_symlink(e.name(), "profile.json", SimpleFileOptions::default())
                            .unwrap();
                    } else {
                        writer
                            .start_file(e.name(), SimpleFileOptions::default())
                            .unwrap();
                        writer.write_all(&bytes).unwrap();
                    }
                }
                nested.1 = writer.finish().unwrap().into_inner();
                let name = nested.0.clone();
                let bytes = nested.1.clone();
                let outer = entries
                    .iter_mut()
                    .find(|(n, _)| n == "manifest.json")
                    .unwrap();
                let mut manifest: BundlePackageManifest = serde_json::from_slice(&outer.1).unwrap();
                let file = manifest.files.iter_mut().find(|f| f.path == name).unwrap();
                file.size = bytes.len() as u64;
                file.sha256 = format!("{:x}", Sha256::digest(&bytes));
                outer.1 = serde_json::to_vec(&manifest).unwrap();
            }
            let path = temp.path().join(format!("link-{nested_link}.hudbundle"));
            let mut writer = ZipWriter::new(File::create(&path).unwrap());
            for (name, bytes) in entries {
                if !nested_link && name == "bundle.json" {
                    writer
                        .add_symlink(&name, "outside", SimpleFileOptions::default())
                        .unwrap();
                } else {
                    writer
                        .start_file(name, SimpleFileOptions::default())
                        .unwrap();
                    writer.write_all(&bytes).unwrap();
                }
            }
            writer.finish().unwrap();
            let store = ProfileStore::new(temp.path().join(format!("target-{nested_link}")), 20);
            assert!(store.import_bundle(&path, ImportLimits::default()).is_err());
            assert!(store.list().unwrap().is_empty());
            assert_eq!(fs::read_dir(store.bundles_root()).unwrap().count(), 0);
        }
    }
    #[test]
    fn collection_count_limit_rejects_publication_before_poisoning_discovery() {
        let temp = tempfile::tempdir().unwrap();
        let (source, bundle) = fixture(&temp.path().join("source"));
        let archive = temp.path().join("queen.hudbundle");
        source
            .export_bundle(bundle.router_profile_id, &archive)
            .unwrap();
        let destination = ProfileStore::new(temp.path().join("destination"), 20);
        fs::create_dir_all(destination.bundles_root()).unwrap();
        for _ in 0..128 {
            fs::create_dir(destination.bundles_root().join(Uuid::new_v4().to_string())).unwrap();
        }
        assert!(destination
            .import_bundle(&archive, ImportLimits::for_bundle())
            .is_err());
        assert_eq!(
            fs::read_dir(destination.bundles_root()).unwrap().count(),
            128
        );
        assert!(destination.list_bundles(None, 16).is_ok());
    }
}
