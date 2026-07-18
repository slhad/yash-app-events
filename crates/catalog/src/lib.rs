//! Validated remote profile catalog, cache, and publication-source boundary.

use std::collections::HashSet;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::StreamExt as _;
use reqwest::{Client, Url};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use uuid::Uuid;
use yash_app_events_output::OutputRecipe;
use yash_app_events_profile::{
    export_profile, load_profile, Detector, Profile, PROFILE_SCHEMA_VERSION,
};

pub const CATALOG_SCHEMA_VERSION: u16 = 1;
pub const DEFAULT_RELEASE_API: &str =
    "https://api.github.com/repos/slhad/yash-app-events/releases/tags/profiles";
pub const DEFAULT_DOWNLOAD_PREFIX: &str =
    "https://github.com/slhad/yash-app-events/releases/download/profiles/";
pub const MAXIMUM_CATALOG_BYTES: usize = 1024 * 1024;
pub const MAXIMUM_PACKAGE_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Catalog {
    pub schema: u16,
    pub revision: u64,
    pub generated_at: String,
    pub profiles: Vec<CatalogEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CatalogEntry {
    pub id: String,
    pub game: String,
    pub game_slug: String,
    pub profile_slug: String,
    pub profile_id: Uuid,
    pub name: String,
    pub description: String,
    pub version: Version,
    pub package: String,
    pub bytes: u64,
    pub sha256: String,
    pub profile_schema: u16,
    pub minimum_app_version: Version,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_app_version: Option<Version>,
    pub media_free: bool,
    #[serde(default)]
    pub detectors: Vec<String>,
    #[serde(default)]
    pub tested_layouts: Vec<TestedLayout>,
    #[serde(default)]
    pub output_recipes: Vec<String>,
    pub license: String,
    pub verification: Verification,
    #[serde(default)]
    pub withdrawn: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TestedLayout {
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_scale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Verification {
    pub status: String,
    pub date: String,
    pub evidence: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceEntry {
    pub schema: u16,
    pub id: String,
    pub game: String,
    pub game_slug: String,
    pub profile_slug: String,
    pub name: String,
    pub description: String,
    pub version: Version,
    pub minimum_app_version: Version,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_app_version: Option<Version>,
    pub media_free: bool,
    #[serde(default)]
    pub detectors: Vec<String>,
    #[serde(default)]
    pub tested_layouts: Vec<TestedLayout>,
    #[serde(default)]
    pub output_recipes: Vec<String>,
    pub license: String,
    pub verification: Verification,
}

#[derive(Clone, Debug, Deserialize)]
struct GithubRelease {
    assets: Vec<GithubAsset>,
}

#[derive(Clone, Debug, Deserialize)]
struct GithubAsset {
    name: String,
    size: u64,
    digest: String,
    browser_download_url: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CatalogStatus {
    pub cached: bool,
    pub revision: Option<u64>,
    pub generated_at: Option<String>,
    pub profile_count: usize,
    pub cache_path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct CatalogService {
    client: Client,
    cache_root: PathBuf,
    release_api: Url,
    download_prefix: String,
}

impl CatalogService {
    /// Creates the production GitHub catalog service rooted in the supplied XDG cache path.
    ///
    /// # Errors
    ///
    /// Returns URL or HTTP-client configuration errors.
    pub fn new(cache_root: PathBuf) -> Result<Self, CatalogError> {
        Self::with_endpoints(cache_root, DEFAULT_RELEASE_API, DEFAULT_DOWNLOAD_PREFIX)
    }

    /// Creates a catalog service with explicit endpoints for isolated integration tests.
    ///
    /// # Errors
    ///
    /// Returns invalid endpoint or HTTP-client configuration errors.
    pub fn with_endpoints(
        cache_root: PathBuf,
        release_api: &str,
        download_prefix: &str,
    ) -> Result<Self, CatalogError> {
        let release_api = Url::parse(release_api)?;
        let download_prefix = Url::parse(download_prefix)?.to_string();
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(3))
            .user_agent(format!("yash-app-events/{}", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            client,
            cache_root,
            release_api,
            download_prefix,
        })
    }

    /// Reports the last validated cached catalog without contacting the network.
    ///
    /// # Errors
    ///
    /// Returns cache I/O, schema, or validation failures.
    pub fn status(&self) -> Result<CatalogStatus, CatalogError> {
        let path = self.catalog_cache_path();
        match self.load() {
            Ok(catalog) => Ok(CatalogStatus {
                cached: true,
                revision: Some(catalog.revision),
                generated_at: Some(catalog.generated_at),
                profile_count: catalog.profiles.len(),
                cache_path: path,
            }),
            Err(CatalogError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(CatalogStatus {
                    cached: false,
                    revision: None,
                    generated_at: None,
                    profile_count: 0,
                    cache_path: path,
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Loads and validates the cached catalog.
    ///
    /// # Errors
    ///
    /// Returns cache I/O, resource-limit, JSON, or validation failures.
    pub fn load(&self) -> Result<Catalog, CatalogError> {
        let bytes = fs::read(self.catalog_cache_path())?;
        if bytes.len() > MAXIMUM_CATALOG_BYTES {
            return Err(CatalogError::Limit("cached catalog exceeds 1 MiB"));
        }
        let catalog: Catalog = serde_json::from_slice(&bytes)?;
        catalog.validate()?;
        Ok(catalog)
    }

    /// Fetches the rolling release, validates its newest index, and atomically caches it.
    ///
    /// # Errors
    ///
    /// Returns HTTP, origin, size, schema, integrity, or cache-write failures.
    pub async fn refresh(&self) -> Result<Catalog, CatalogError> {
        let release_bytes = self
            .download_bounded(self.release_api.clone(), MAXIMUM_CATALOG_BYTES)
            .await?;
        let release: GithubRelease = serde_json::from_slice(&release_bytes)?;
        let asset = release
            .assets
            .iter()
            .filter_map(|asset| catalog_revision(&asset.name).map(|revision| (revision, asset)))
            .max_by_key(|(revision, _)| *revision)
            .map(|(_, asset)| asset)
            .ok_or(CatalogError::Invalid("release has no catalog revision"))?;
        if asset.size > MAXIMUM_CATALOG_BYTES as u64 {
            return Err(CatalogError::Limit("remote catalog exceeds 1 MiB"));
        }
        self.validate_download_url(&asset.browser_download_url, &asset.name)?;
        let bytes = self
            .download_bounded(
                Url::parse(&asset.browser_download_url)?,
                MAXIMUM_CATALOG_BYTES,
            )
            .await?;
        if bytes.len() as u64 != asset.size || asset.digest != format!("sha256:{}", digest(&bytes))
        {
            return Err(CatalogError::Integrity(
                "catalog release size or digest mismatch",
            ));
        }
        let catalog: Catalog = serde_json::from_slice(&bytes)?;
        catalog.validate()?;
        if catalog_revision(&asset.name) != Some(catalog.revision) {
            return Err(CatalogError::Invalid(
                "catalog filename and document revision differ",
            ));
        }
        atomic_write(&self.catalog_cache_path(), &bytes)?;
        Ok(catalog)
    }

    /// Downloads and verifies one immutable catalog package into the cache.
    ///
    /// # Errors
    ///
    /// Returns withdrawn/invalid metadata, HTTP, size, digest, or cache-write failures.
    pub async fn download_package(&self, entry: &CatalogEntry) -> Result<PathBuf, CatalogError> {
        entry.validate()?;
        if entry.withdrawn {
            return Err(CatalogError::Invalid("profile version is withdrawn"));
        }
        let url = format!("{}{}", self.download_prefix, entry.package);
        self.validate_download_url(&url, &entry.package)?;
        let maximum = usize::try_from(entry.bytes)
            .unwrap_or(usize::MAX)
            .min(MAXIMUM_PACKAGE_BYTES);
        if maximum == 0 || entry.bytes > MAXIMUM_PACKAGE_BYTES as u64 {
            return Err(CatalogError::Limit("package size is invalid"));
        }
        let bytes = self.download_bounded(Url::parse(&url)?, maximum).await?;
        if bytes.len() as u64 != entry.bytes {
            return Err(CatalogError::Integrity("package size mismatch"));
        }
        if digest(&bytes) != entry.sha256 {
            return Err(CatalogError::Integrity("package SHA-256 mismatch"));
        }
        let path = self.cache_root.join("packages").join(&entry.package);
        atomic_write(&path, &bytes)?;
        Ok(path)
    }

    async fn download_bounded(&self, url: Url, maximum: usize) -> Result<Vec<u8>, CatalogError> {
        let response = self.client.get(url).send().await?.error_for_status()?;
        if response
            .content_length()
            .is_some_and(|length| length > maximum as u64)
        {
            return Err(CatalogError::Limit("HTTP response exceeds declared limit"));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if bytes.len().saturating_add(chunk.len()) > maximum {
                return Err(CatalogError::Limit("HTTP response exceeds declared limit"));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    fn validate_download_url(&self, url: &str, filename: &str) -> Result<(), CatalogError> {
        if url != format!("{}{}", self.download_prefix, filename) {
            return Err(CatalogError::Invalid("release asset URL is not trusted"));
        }
        Ok(())
    }

    fn catalog_cache_path(&self) -> PathBuf {
        self.cache_root.join("catalog.json")
    }
}

impl Catalog {
    /// Validates the complete catalog contract and entry uniqueness.
    ///
    /// # Errors
    ///
    /// Returns the first schema, metadata, package, or uniqueness violation.
    pub fn validate(&self) -> Result<(), CatalogError> {
        if self.schema != CATALOG_SCHEMA_VERSION || self.revision == 0 {
            return Err(CatalogError::Invalid(
                "unsupported catalog schema or revision",
            ));
        }
        if self.generated_at.trim().is_empty() || self.profiles.len() > 256 {
            return Err(CatalogError::Invalid("catalog metadata is invalid"));
        }
        let mut identities = HashSet::new();
        let mut packages = HashSet::new();
        let mut profile_ids = HashSet::new();
        for entry in &self.profiles {
            entry.validate()?;
            if !identities.insert((entry.id.as_str(), &entry.version))
                || !packages.insert(entry.package.as_str())
                || !profile_ids.insert(entry.profile_id)
            {
                return Err(CatalogError::Invalid("catalog entries are not unique"));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn compatible_profiles(&self, application: &Version) -> Vec<CatalogEntry> {
        self.profiles
            .iter()
            .filter(|entry| entry.compatible_with(application))
            .cloned()
            .collect()
    }
}

impl CatalogEntry {
    /// Validates one published catalog entry.
    ///
    /// # Errors
    ///
    /// Returns invalid identity, compatibility, integrity, or package metadata.
    pub fn validate(&self) -> Result<(), CatalogError> {
        validate_source_fields(
            self.id.as_str(),
            self.game.as_str(),
            self.game_slug.as_str(),
            self.profile_slug.as_str(),
            self.name.as_str(),
            self.description.as_str(),
            &self.version,
            &self.minimum_app_version,
            self.maximum_app_version.as_ref(),
            self.license.as_str(),
            &self.verification,
        )?;
        if self.profile_schema != PROFILE_SCHEMA_VERSION
            || self.bytes == 0
            || self.bytes > MAXIMUM_PACKAGE_BYTES as u64
            || !is_sha256(&self.sha256)
            || self.package != package_name(&self.game_slug, &self.profile_slug, &self.version)
        {
            return Err(CatalogError::Invalid("catalog package metadata is invalid"));
        }
        Ok(())
    }

    #[must_use]
    pub fn compatible_with(&self, application: &Version) -> bool {
        !self.withdrawn
            && application >= &self.minimum_app_version
            && self
                .maximum_app_version
                .as_ref()
                .is_none_or(|maximum| application <= maximum)
    }
}

impl SourceEntry {
    /// Validates one reviewable repository source entry.
    ///
    /// # Errors
    ///
    /// Returns invalid identity, compatibility, provenance, or verification metadata.
    pub fn validate(&self) -> Result<(), CatalogError> {
        if self.schema != CATALOG_SCHEMA_VERSION {
            return Err(CatalogError::Invalid("unsupported source-entry schema"));
        }
        validate_source_fields(
            &self.id,
            &self.game,
            &self.game_slug,
            &self.profile_slug,
            &self.name,
            &self.description,
            &self.version,
            &self.minimum_app_version,
            self.maximum_app_version.as_ref(),
            &self.license,
            &self.verification,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_source_fields(
    id: &str,
    game: &str,
    game_slug: &str,
    profile_slug: &str,
    name: &str,
    description: &str,
    version: &Version,
    minimum: &Version,
    maximum: Option<&Version>,
    license: &str,
    verification: &Verification,
) -> Result<(), CatalogError> {
    if !safe_identifier(id, true)
        || !safe_identifier(game, false)
        || !safe_identifier(game_slug, false)
        || !safe_identifier(profile_slug, false)
        || id != format!("{game_slug}.{profile_slug}")
        || name.trim().is_empty()
        || name.len() > 128
        || description.len() > 4096
        || license.trim().is_empty()
        || version.to_string().len() > 64
        || version.pre.len() > 32
        || maximum.is_some_and(|maximum| maximum < minimum)
        || verification.status != "verified"
        || verification.date.trim().is_empty()
        || verification.evidence.trim().is_empty()
    {
        return Err(CatalogError::Invalid("catalog entry metadata is invalid"));
    }
    Ok(())
}

fn safe_identifier(value: &str, dots: bool) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || byte == b'-'
                || byte == b'_'
                || (dots && byte == b'.')
        })
        && !value.starts_with(['-', '_', '.'])
        && !value.ends_with(['-', '_', '.'])
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[must_use]
pub fn package_name(game_slug: &str, profile_slug: &str, version: &Version) -> String {
    format!("profile--{game_slug}--{profile_slug}--v{version}.hudprofile")
}

#[must_use]
pub fn catalog_filename(revision: u64) -> String {
    format!("catalog-v1-r{revision:06}.json")
}

fn catalog_revision(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("catalog-v1-r")?.strip_suffix(".json")?;
    (digits.len() == 6 && digits.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

/// Reads and validates a repository source entry.
///
/// # Errors
///
/// Returns file, JSON, or source metadata validation failures.
pub fn read_source_entry(directory: &Path) -> Result<SourceEntry, CatalogError> {
    let source: SourceEntry =
        serde_json::from_slice(&fs::read(directory.join("catalog-entry.json"))?)?;
    source.validate()?;
    Ok(source)
}

/// Audits a source directory and loads its validated portable profile.
///
/// # Errors
///
/// Returns unsafe inventory, PII/secret, profile, recipe, or metadata failures.
pub fn validate_source_directory(directory: &Path) -> Result<(SourceEntry, Profile), CatalogError> {
    let source = read_source_entry(directory)?;
    let profile = load_profile(&directory.join("profile.json"))?;
    if profile.game != source.game
        || profile.layout.reference_width == 0
        || profile.layout.reference_height == 0
    {
        return Err(CatalogError::Invalid(
            "source profile metadata differs from catalog entry",
        ));
    }
    validate_source_path(directory, &source)?;
    let mut actual_detectors = profile
        .elements
        .iter()
        .map(|element| match element.detector {
            Detector::ColorBar { .. } => "color_bar",
            Detector::Template { .. } => "template",
            Detector::RegionChange { .. } => "region_change",
            Detector::Ocr { .. } => "ocr",
            Detector::SevenSegment { .. } => "seven_segment",
            Detector::Classifier { .. } => "classifier",
        })
        .collect::<Vec<_>>();
    actual_detectors.sort_unstable();
    actual_detectors.dedup();
    let mut declared_detectors = source
        .detectors
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    declared_detectors.sort_unstable();
    declared_detectors.dedup();
    if actual_detectors != declared_detectors {
        return Err(CatalogError::Invalid(
            "declared detectors differ from the profile",
        ));
    }
    let mut actual_recipes = load_source_recipes(directory)?
        .into_iter()
        .map(|recipe| recipe.name)
        .collect::<Vec<_>>();
    actual_recipes.sort();
    let mut declared_recipes = source.output_recipes.clone();
    declared_recipes.sort();
    if actual_recipes != declared_recipes {
        return Err(CatalogError::Invalid(
            "declared output recipes differ from source files",
        ));
    }
    audit_source_files(directory, source.media_free)?;
    Ok((source, profile))
}

fn validate_source_path(directory: &Path, source: &SourceEntry) -> Result<(), CatalogError> {
    let version = format!("v{}", source.version);
    let parts = [
        version.as_str(),
        source.profile_slug.as_str(),
        source.game_slug.as_str(),
    ];
    let mut current = directory;
    for expected in parts {
        if current.file_name().and_then(|name| name.to_str()) != Some(expected) {
            return Err(CatalogError::Invalid(
                "source directory does not match catalog identity/version",
            ));
        }
        current = current
            .parent()
            .ok_or(CatalogError::Invalid("source directory is incomplete"))?;
    }
    Ok(())
}

fn load_source_recipes(directory: &Path) -> Result<Vec<OutputRecipe>, CatalogError> {
    let recipes = directory.join("output-recipes");
    if !recipes.is_dir() {
        return Ok(Vec::new());
    }
    let mut values = Vec::new();
    for entry in fs::read_dir(recipes)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            return Err(CatalogError::UnsafeSource(
                entry.path().display().to_string(),
            ));
        }
        let recipe: OutputRecipe = serde_json::from_slice(&fs::read(entry.path())?)?;
        recipe
            .validate()
            .map_err(|_| CatalogError::Invalid("source output recipe is invalid"))?;
        values.push(recipe);
    }
    Ok(values)
}

fn audit_source_files(directory: &Path, media_free: bool) -> Result<(), CatalogError> {
    let mut files = Vec::new();
    collect_files(directory, directory, &mut files)?;
    for relative in files {
        let normalized = relative.to_string_lossy().replace('\\', "/");
        let allowed = normalized == "profile.json"
            || normalized == "catalog-entry.json"
            || normalized == "verification.json"
            || (normalized.starts_with("output-recipes/")
                && Path::new(&normalized)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("json")));
        if !allowed {
            return Err(CatalogError::UnsafeSource(normalized));
        }
        if media_free && is_media_path(&normalized) {
            return Err(CatalogError::UnsafeSource(normalized));
        }
        let bytes = fs::read(directory.join(&relative))?;
        let text =
            String::from_utf8(bytes).map_err(|_| CatalogError::UnsafeSource(normalized.clone()))?;
        let value: serde_json::Value = serde_json::from_str(&text)?;
        audit_json_value(&normalized, &value)?;
        let lowered = text.to_ascii_lowercase();
        if lowered.contains("restore_token")
            || lowered.contains("capture-bindings")
            || lowered.contains("output-routes")
            || lowered.contains("/home/")
            || lowered.contains("file://")
        {
            return Err(CatalogError::UnsafeSource(normalized));
        }
    }
    Ok(())
}

fn audit_json_value(path: &str, value: &serde_json::Value) -> Result<(), CatalogError> {
    match value {
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                let lowered = key.to_ascii_lowercase();
                if [
                    "password",
                    "secret",
                    "token",
                    "credential",
                    "capture_binding",
                    "output_route",
                ]
                .iter()
                .any(|forbidden| lowered.contains(forbidden))
                {
                    return Err(CatalogError::UnsafeSource(format!("{path}:{key}")));
                }
                audit_json_value(path, value)?;
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                audit_json_value(path, value)?;
            }
        }
        serde_json::Value::String(value) => {
            if Path::new(value).is_absolute()
                || value.split_whitespace().any(|word| {
                    let trimmed = word.trim_matches(|character: char| {
                        !character.is_ascii_alphanumeric() && character != '@' && character != '.'
                    });
                    trimmed.contains('@') && trimmed.rsplit_once('.').is_some()
                })
            {
                return Err(CatalogError::UnsafeSource(path.into()));
            }
        }
        _ => {}
    }
    Ok(())
}

fn collect_files(
    root: &Path,
    current: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), CatalogError> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(CatalogError::UnsafeSource(
                entry.path().display().to_string(),
            ));
        }
        if file_type.is_dir() {
            collect_files(root, &entry.path(), files)?;
        } else if file_type.is_file() {
            files.push(
                entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|_| CatalogError::Invalid("source path escaped root"))?
                    .to_path_buf(),
            );
        }
    }
    Ok(())
}

fn is_media_path(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some(
            "png"
                | "jpg"
                | "jpeg"
                | "webp"
                | "gif"
                | "mp4"
                | "webm"
                | "mkv"
                | "wav"
                | "ogg"
                | "mp3"
        )
    )
}

/// Builds one immutable `.hudprofile` from audited repository source documents.
///
/// # Errors
///
/// Returns source audit, staging, archive, or output I/O failures.
pub fn build_package(source_directory: &Path, output: &Path) -> Result<CatalogEntry, CatalogError> {
    let (source, profile) = validate_source_directory(source_directory)?;
    let staging = tempfile_directory(output)?;
    let result = (|| {
        fs::copy(
            source_directory.join("profile.json"),
            staging.join("profile.json"),
        )?;
        let recipes = source_directory.join("output-recipes");
        if recipes.is_dir() {
            copy_directory(&recipes, &staging.join("output-recipes"))?;
        }
        let filename = package_name(&source.game_slug, &source.profile_slug, &source.version);
        fs::create_dir_all(output)?;
        let destination = output.join(&filename);
        export_profile(&staging, &destination)?;
        let bytes = fs::read(&destination)?;
        Ok::<_, CatalogError>((filename, bytes))
    })();
    let _ = fs::remove_dir_all(&staging);
    let (filename, bytes) = result?;
    Ok(CatalogEntry {
        id: source.id,
        game: source.game,
        game_slug: source.game_slug,
        profile_slug: source.profile_slug,
        profile_id: Uuid::parse_str(&profile.id.to_string())
            .map_err(|_| CatalogError::Invalid("profile ID is not a UUID"))?,
        name: source.name,
        description: source.description,
        version: source.version,
        package: filename,
        bytes: bytes.len() as u64,
        sha256: digest(&bytes),
        profile_schema: profile.schema,
        minimum_app_version: source.minimum_app_version,
        maximum_app_version: source.maximum_app_version,
        media_free: source.media_free,
        detectors: source.detectors,
        tested_layouts: source.tested_layouts,
        output_recipes: source.output_recipes,
        license: source.license,
        verification: source.verification,
        withdrawn: false,
    })
}

fn tempfile_directory(output: &Path) -> Result<PathBuf, CatalogError> {
    let parent = output.parent().unwrap_or(output);
    fs::create_dir_all(parent)?;
    let path = parent.join(format!(".catalog-build-{}", Uuid::new_v4()));
    fs::create_dir(&path)?;
    Ok(path)
}

fn copy_directory(source: &Path, destination: &Path) -> Result<(), CatalogError> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            return Err(CatalogError::UnsafeSource(
                entry.path().display().to_string(),
            ));
        }
        fs::copy(entry.path(), destination.join(entry.file_name()))?;
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), CatalogError> {
    let parent = path
        .parent()
        .ok_or(CatalogError::Invalid("cache path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok::<(), std::io::Error>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(CatalogError::from)
}

#[must_use]
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("catalog I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("catalog JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("catalog HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("catalog URL is invalid: {0}")]
    Url(#[from] url::ParseError),
    #[error("profile source is unsafe: {0}")]
    UnsafeSource(String),
    #[error("catalog resource limit exceeded: {0}")]
    Limit(&'static str),
    #[error("catalog integrity check failed: {0}")]
    Integrity(&'static str),
    #[error("catalog is invalid: {0}")]
    Invalid(&'static str),
    #[error("profile archive failed: {0}")]
    Archive(#[from] yash_app_events_profile::ArchiveError),
    #[error("profile storage failed: {0}")]
    Profile(#[from] yash_app_events_profile::StorageError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    fn entry() -> CatalogEntry {
        CatalogEntry {
            id: "demo-game.stage-tracker".into(),
            game: "demo_game".into(),
            game_slug: "demo-game".into(),
            profile_slug: "stage-tracker".into(),
            profile_id: Uuid::nil(),
            name: "Stage tracker".into(),
            description: "Portable stage detector".into(),
            version: Version::new(1, 0, 0),
            package: "profile--demo-game--stage-tracker--v1.0.0.hudprofile".into(),
            bytes: 42,
            sha256: "0".repeat(64),
            profile_schema: 1,
            minimum_app_version: Version::new(0, 0, 1),
            maximum_app_version: None,
            media_free: true,
            detectors: vec!["ocr".into()],
            tested_layouts: Vec::new(),
            output_recipes: Vec::new(),
            license: "MIT".into(),
            verification: Verification {
                status: "verified".into(),
                date: "2026-07-18".into(),
                evidence: "private replay passed".into(),
            },
            withdrawn: false,
        }
    }

    #[test]
    fn entry_filename_and_compatibility_are_strict() {
        let mut value = entry();
        value.validate().unwrap();
        assert!(value.compatible_with(&Version::new(0, 0, 1)));
        value.package = "other.hudprofile".into();
        assert!(value.validate().is_err());
    }

    #[test]
    fn catalog_rejects_duplicate_versions_and_profiles() {
        let value = entry();
        let catalog = Catalog {
            schema: 1,
            revision: 1,
            generated_at: "2026-07-18T00:00:00Z".into(),
            profiles: vec![value.clone(), value],
        };
        assert!(catalog.validate().is_err());
    }

    #[test]
    fn revision_filename_parser_ignores_unrelated_assets() {
        assert_eq!(catalog_revision("catalog-v1-r000042.json"), Some(42));
        assert_eq!(catalog_revision("profile.hudprofile"), None);
    }

    #[test]
    fn media_free_source_builds_without_publication_metadata() {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../catalog/profiles/blazblue-entropy-effect/stage-tracker/v1.0.0");
        let output = tempfile::tempdir().unwrap();
        let entry = build_package(&source, output.path()).unwrap();
        assert!(entry.media_free);
        assert_eq!(
            entry.profile_id.to_string(),
            "ae765dfd-9ecf-4ac3-a9dd-c8d1c912879b"
        );
        let installed = tempfile::tempdir().unwrap();
        let data_root = installed.path().join("data");
        let profiles_root = data_root.join("profiles");
        let profile = yash_app_events_profile::import_profile(
            &output.path().join(&entry.package),
            &profiles_root,
            yash_app_events_profile::ImportLimits::default(),
        )
        .unwrap();
        let root = profiles_root.join(profile.id.to_string());
        assert!(!root.join("catalog-entry.json").exists());
        assert!(!root.join("verification.json").exists());
        assert_eq!(
            yash_app_events_profile::ProfileStore::new(data_root, 20)
                .output_recipes(profile.id)
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn refresh_cache_and_package_download_enforce_release_contract() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let prefix = format!("http://{address}/download/");
        let package = b"verified portable package".to_vec();
        let mut profile = entry();
        profile.bytes = package.len() as u64;
        profile.sha256 = digest(&package);
        let catalog = Catalog {
            schema: 1,
            revision: 7,
            generated_at: "2026-07-18T00:00:00Z".into(),
            profiles: vec![profile.clone()],
        };
        let catalog_bytes = serde_json::to_vec(&catalog).unwrap();
        let catalog_name = catalog_filename(7);
        let release = serde_json::to_vec(&serde_json::json!({
            "assets": [{
                "name": catalog_name,
                "size": catalog_bytes.len(),
                "digest": format!("sha256:{}", digest(&catalog_bytes)),
                "browser_download_url": format!("{prefix}{catalog_name}")
            }]
        }))
        .unwrap();
        let catalog_path = format!("/download/{catalog_name}");
        let package_path = format!("/download/{}", profile.package);
        let expected_package = package.clone();
        let server = tokio::spawn(async move {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = vec![0_u8; 4096];
                let count = stream.read(&mut request).await.unwrap();
                let first = String::from_utf8_lossy(&request[..count]);
                let path = first.split_whitespace().nth(1).unwrap();
                let body = match path {
                    "/release" => &release,
                    value if value == catalog_path => &catalog_bytes,
                    value if value == package_path => &package,
                    _ => panic!("unexpected request path: {path}"),
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/json\r\n\r\n",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.write_all(body).await.unwrap();
            }
        });
        let cache = tempfile::tempdir().unwrap();
        let service = CatalogService::with_endpoints(
            cache.path().to_path_buf(),
            &format!("http://{address}/release"),
            &prefix,
        )
        .unwrap();
        assert_eq!(service.refresh().await.unwrap(), catalog);
        assert_eq!(service.load().unwrap(), catalog);
        let downloaded = service.download_package(&profile).await.unwrap();
        assert_eq!(fs::read(downloaded).unwrap(), expected_package);
        server.await.unwrap();
    }
}
