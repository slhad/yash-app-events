use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use uuid::Uuid;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::{load_profile, Profile, ProfileId, StorageError, PROFILE_SCHEMA_VERSION};

const MANIFEST_FILE: &str = "manifest.json";
const PROFILE_FILE: &str = "profile.json";

/// Export archive manifest version one.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Manifest {
    pub schema: u16,
    pub profile_schema: u16,
    pub profile_id: ProfileId,
    pub files: Vec<ManifestFile>,
}

/// Integrity and resource metadata for one portable file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ManifestFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// Hard resource limits applied before and during extraction.
#[derive(Clone, Copy, Debug)]
pub struct ImportLimits {
    pub maximum_files: usize,
    pub maximum_file_bytes: u64,
    pub maximum_total_bytes: u64,
}

impl Default for ImportLimits {
    fn default() -> Self {
        Self {
            maximum_files: 256,
            maximum_file_bytes: 32 * 1024 * 1024,
            maximum_total_bytes: 128 * 1024 * 1024,
        }
    }
}

/// Exports a self-contained portable profile directory.
///
/// # Errors
///
/// Rejects symlinks, unsafe relative names, missing assets, invalid profiles, and I/O failures.
pub fn export_profile(
    profile_directory: &Path,
    destination: &Path,
) -> Result<Manifest, ArchiveError> {
    let profile = load_profile(&profile_directory.join(PROFILE_FILE))?;
    validate_declared_assets(&profile, profile_directory)?;
    let mut paths = Vec::new();
    collect_portable_files(profile_directory, profile_directory, &mut paths)?;
    paths.sort();
    let mut entries = Vec::with_capacity(paths.len());
    for relative in &paths {
        let bytes = fs::read(profile_directory.join(relative))?;
        entries.push(ManifestFile {
            path: portable_name(relative)?,
            size: bytes.len() as u64,
            sha256: digest(&bytes),
        });
    }
    let manifest = Manifest {
        schema: 1,
        profile_schema: profile.schema,
        profile_id: profile.id,
        files: entries,
    };
    let parent = destination.parent().ok_or(ArchiveError::UnsafePath)?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = write_archive(&temporary, profile_directory, &paths, &manifest)
        .and_then(|()| fs::rename(&temporary, destination).map_err(ArchiveError::from));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map(|()| manifest)
}

fn write_archive(
    path: &Path,
    root: &Path,
    paths: &[PathBuf],
    manifest: &Manifest,
) -> Result<(), ArchiveError> {
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o600);
    writer.start_file(MANIFEST_FILE, options)?;
    writer.write_all(&serde_json::to_vec_pretty(manifest)?)?;
    for relative in paths {
        writer.start_file(portable_name(relative)?, options)?;
        writer.write_all(&fs::read(root.join(relative))?)?;
    }
    let file = writer.finish()?;
    file.sync_all()?;
    Ok(())
}

/// Safely imports an archive into a profiles root and returns its validated profile.
///
/// # Errors
///
/// Rejects unsafe paths, links, duplicate or undeclared entries, integrity mismatches,
/// resource-limit violations, unsupported schemas, collisions, and malformed profiles.
pub fn import_profile(
    archive_path: &Path,
    profiles_root: &Path,
    limits: ImportLimits,
) -> Result<Profile, ArchiveError> {
    fs::create_dir_all(profiles_root)?;
    let mut archive = ZipArchive::new(File::open(archive_path)?)?;
    if archive.len() > limits.maximum_files.saturating_add(1) {
        return Err(ArchiveError::LimitExceeded("file count"));
    }
    let manifest: Manifest = {
        let mut entry = archive
            .by_name(MANIFEST_FILE)
            .map_err(|_| ArchiveError::MissingManifest)?;
        if entry.size() > limits.maximum_file_bytes {
            return Err(ArchiveError::LimitExceeded("manifest size"));
        }
        serde_json::from_reader(&mut entry)?
    };
    if manifest.schema != 1 || manifest.profile_schema != PROFILE_SCHEMA_VERSION {
        return Err(ArchiveError::UnsupportedSchema);
    }
    let declared: HashMap<_, _> = manifest
        .files
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    if declared.len() != manifest.files.len() || !declared.contains_key(PROFILE_FILE) {
        return Err(ArchiveError::InvalidManifest);
    }
    let destination = profiles_root.join(manifest.profile_id.to_string());
    if destination.exists() {
        return Err(ArchiveError::AlreadyExists(manifest.profile_id));
    }
    let staging = profiles_root.join(format!(".import-{}.tmp", Uuid::new_v4()));
    fs::create_dir(&staging)?;
    let result = extract_archive(&mut archive, &manifest, &staging, limits).and_then(|()| {
        let profile = load_profile(&staging.join(PROFILE_FILE))?;
        if profile.id != manifest.profile_id {
            return Err(ArchiveError::InvalidManifest);
        }
        validate_declared_assets(&profile, &staging)?;
        fs::rename(&staging, &destination)?;
        Ok(profile)
    });
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn extract_archive(
    archive: &mut ZipArchive<File>,
    manifest: &Manifest,
    staging: &Path,
    limits: ImportLimits,
) -> Result<(), ArchiveError> {
    let declared: HashMap<_, _> = manifest
        .files
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let mut seen = HashSet::new();
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let name = entry.name().to_owned();
        if name == MANIFEST_FILE {
            continue;
        }
        if entry.is_dir()
            || entry.enclosed_name().is_none()
            || is_symlink(entry.unix_mode())
            || !safe_portable_name(&name)
        {
            return Err(ArchiveError::UnsafeEntry(name));
        }
        if !seen.insert(name.clone()) {
            return Err(ArchiveError::DuplicateEntry(name));
        }
        let expected = declared
            .get(name.as_str())
            .ok_or_else(|| ArchiveError::UndeclaredEntry(name.clone()))?;
        if entry.size() != expected.size || entry.size() > limits.maximum_file_bytes {
            return Err(ArchiveError::LimitExceeded("individual file size"));
        }
        total = total
            .checked_add(entry.size())
            .ok_or(ArchiveError::LimitExceeded("total size"))?;
        if total > limits.maximum_total_bytes {
            return Err(ArchiveError::LimitExceeded("total size"));
        }
        let output = staging.join(&name);
        fs::create_dir_all(output.parent().ok_or(ArchiveError::UnsafePath)?)?;
        let capacity = usize::try_from(entry.size())
            .map_err(|_| ArchiveError::LimitExceeded("addressable file size"))?;
        let mut bytes = Vec::with_capacity(capacity);
        entry
            .by_ref()
            .take(limits.maximum_file_bytes + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != expected.size || digest(&bytes) != expected.sha256 {
            return Err(ArchiveError::Integrity(name));
        }
        fs::write(output, bytes)?;
    }
    if seen.len() != declared.len() {
        return Err(ArchiveError::MissingDeclaredFile);
    }
    Ok(())
}

fn collect_portable_files(
    root: &Path,
    directory: &Path,
    paths: &mut Vec<PathBuf>,
) -> Result<(), ArchiveError> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|_| ArchiveError::UnsafePath)?
            .to_owned();
        if relative
            .components()
            .next()
            .is_some_and(|part| part.as_os_str() == "revisions")
            || relative == Path::new("draft.json")
        {
            continue;
        }
        if file_type.is_symlink() {
            return Err(ArchiveError::UnsafeEntry(portable_name(&relative)?));
        }
        if file_type.is_dir() {
            collect_portable_files(root, &entry.path(), paths)?;
        } else if file_type.is_file() {
            paths.push(relative);
        }
    }
    Ok(())
}

fn validate_declared_assets(profile: &Profile, root: &Path) -> Result<(), ArchiveError> {
    for element in &profile.elements {
        let assets: Vec<&PathBuf> = match &element.detector {
            crate::Detector::Template {
                templates, masks, ..
            } => templates.iter().chain(masks.iter().flatten()).collect(),
            crate::Detector::ColorBar {
                mask: Some(mask), ..
            } => vec![mask],
            crate::Detector::Classifier { model, .. } => vec![model],
            _ => Vec::new(),
        };
        for asset in assets {
            if !root.join(asset).is_file() {
                return Err(ArchiveError::MissingAsset(asset.clone()));
            }
        }
    }
    Ok(())
}

fn safe_portable_name(name: &str) -> bool {
    !name.is_empty()
        && Path::new(name)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        && !name.contains('\\')
}

fn portable_name(path: &Path) -> Result<String, ArchiveError> {
    let name = path
        .to_str()
        .ok_or(ArchiveError::UnsafePath)?
        .replace('\\', "/");
    safe_portable_name(&name)
        .then_some(name)
        .ok_or(ArchiveError::UnsafePath)
}

fn is_symlink(mode: Option<u32>) -> bool {
    mode.is_some_and(|mode| mode & 0o170_000 == 0o120_000)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Safe import/export failure.
#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("archive I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("ZIP archive failed: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("archive JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("archive path is unsafe")]
    UnsafePath,
    #[error("unsafe archive entry: {0}")]
    UnsafeEntry(String),
    #[error("duplicate archive entry: {0}")]
    DuplicateEntry(String),
    #[error("undeclared archive entry: {0}")]
    UndeclaredEntry(String),
    #[error("archive integrity mismatch: {0}")]
    Integrity(String),
    #[error("archive resource limit exceeded: {0}")]
    LimitExceeded(&'static str),
    #[error("archive manifest is missing")]
    MissingManifest,
    #[error("archive manifest is invalid")]
    InvalidManifest,
    #[error("archive uses an unsupported schema")]
    UnsupportedSchema,
    #[error("archive omits a declared file")]
    MissingDeclaredFile,
    #[error("portable asset is missing: {0}")]
    MissingAsset(PathBuf),
    #[error("profile {0} already exists")]
    AlreadyExists(ProfileId),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_profile(root: &Path) -> Profile {
        let profile = Profile::new("Demo", "demo_game", 1920, 1080);
        fs::create_dir_all(root).unwrap();
        crate::save_profile(&root.join(PROFILE_FILE), &profile).unwrap();
        fs::create_dir(root.join("templates")).unwrap();
        fs::write(root.join("templates/icon.bin"), b"portable asset").unwrap();
        profile
    }

    #[test]
    fn archive_round_trip_checks_manifest_and_assets() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let profile = source_profile(&source);
        let archive = directory.path().join("demo.hudprofile");
        let manifest = export_profile(&source, &archive).unwrap();
        assert_eq!(manifest.profile_id, profile.id);
        assert_eq!(manifest.files.len(), 2);
        let imported = import_profile(
            &archive,
            &directory.path().join("profiles"),
            ImportLimits::default(),
        )
        .unwrap();
        assert_eq!(imported, profile);
        assert_eq!(
            fs::read(
                directory
                    .path()
                    .join("profiles")
                    .join(profile.id.to_string())
                    .join("templates/icon.bin")
            )
            .unwrap(),
            b"portable asset"
        );
    }

    #[test]
    fn classifier_model_is_a_required_portable_asset() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let mut profile = Profile::new("Classifier", "demo_game", 8, 8);
        profile.elements.push(crate::Element {
            id: crate::ElementId::new(),
            name: "Icon".into(),
            enabled: true,
            color: "#fff".into(),
            region: crate::NormalizedRegion {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            detector: crate::Detector::Classifier {
                id: crate::DetectorId::new(),
                model: "models/icon.onnx".into(),
                model_sha256: "0".repeat(64),
                labels: vec!["absent".into(), "present".into()],
                input_width: 8,
                input_height: 8,
                preprocessing: Vec::new(),
                change_trigger_threshold: 0.02,
                maximum_interval_ms: 1_000,
            },
        });
        fs::create_dir_all(&source).unwrap();
        crate::save_profile(&source.join(PROFILE_FILE), &profile).unwrap();
        let error =
            export_profile(&source, &directory.path().join("profile.hudprofile")).unwrap_err();
        assert_eq!(
            error.to_string(),
            "portable asset is missing: models/icon.onnx"
        );
    }

    #[test]
    fn traversal_entry_is_rejected_without_escape_or_partial_publish() {
        let directory = tempfile::tempdir().unwrap();
        let profile = Profile::new("Demo", "demo_game", 1, 1);
        let archive = directory.path().join("evil.zip");
        write_custom_archive(&archive, &profile, "../escaped", b"evil", 0o600);
        let profiles = directory.path().join("profiles");
        let error = import_profile(&archive, &profiles, ImportLimits::default()).unwrap_err();
        assert!(matches!(error, ArchiveError::UnsafeEntry(_)));
        assert!(!directory.path().join("escaped").exists());
        assert!(!profiles.join(profile.id.to_string()).exists());
    }

    #[test]
    fn symlink_entry_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let profile = Profile::new("Demo", "demo_game", 1, 1);
        let archive = directory.path().join("link.zip");
        write_custom_archive(&archive, &profile, "link", b"target", 0o120_777);
        let error = import_profile(
            &archive,
            &directory.path().join("profiles"),
            ImportLimits::default(),
        )
        .unwrap_err();
        assert!(matches!(error, ArchiveError::UnsafeEntry(_)));
    }

    #[test]
    fn expansion_limit_is_checked_before_extraction() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        source_profile(&source);
        fs::write(source.join("large.bin"), vec![0_u8; 1024]).unwrap();
        let archive = directory.path().join("large.zip");
        export_profile(&source, &archive).unwrap();
        let error = import_profile(
            &archive,
            &directory.path().join("profiles"),
            ImportLimits {
                maximum_files: 10,
                maximum_file_bytes: 512,
                maximum_total_bytes: 2048,
            },
        )
        .unwrap_err();
        assert!(matches!(error, ArchiveError::LimitExceeded(_)));
    }

    fn write_custom_archive(
        path: &Path,
        profile: &Profile,
        extra_name: &str,
        extra: &[u8],
        permissions: u32,
    ) {
        let profile_bytes = serde_json::to_vec_pretty(profile).unwrap();
        let manifest = Manifest {
            schema: 1,
            profile_schema: 1,
            profile_id: profile.id,
            files: vec![
                ManifestFile {
                    path: PROFILE_FILE.into(),
                    size: profile_bytes.len() as u64,
                    sha256: digest(&profile_bytes),
                },
                ManifestFile {
                    path: extra_name.into(),
                    size: extra.len() as u64,
                    sha256: digest(extra),
                },
            ],
        };
        let mut writer = ZipWriter::new(File::create(path).unwrap());
        let regular = SimpleFileOptions::default().unix_permissions(0o600);
        writer.start_file(MANIFEST_FILE, regular).unwrap();
        writer
            .write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        writer.start_file(PROFILE_FILE, regular).unwrap();
        writer.write_all(&profile_bytes).unwrap();
        if permissions & 0o170_000 == 0o120_000 {
            writer
                .add_symlink(
                    extra_name,
                    String::from_utf8_lossy(extra),
                    SimpleFileOptions::default().unix_permissions(0o777),
                )
                .unwrap();
        } else {
            writer
                .start_file(
                    extra_name,
                    SimpleFileOptions::default().unix_permissions(permissions),
                )
                .unwrap();
            writer.write_all(extra).unwrap();
        }
        writer.finish().unwrap();
    }
}
