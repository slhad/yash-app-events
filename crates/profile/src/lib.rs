//! Portable profile schemas, validation, migration, and durable local storage.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Current portable profile schema version.
pub const PROFILE_SCHEMA_VERSION: u16 = 1;

macro_rules! opaque_id {
    ($name:ident) => {
        #[doc = "Stable opaque identity persisted in profiles and external contracts."]
        #[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Allocates a random version-four identity.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

opaque_id!(ProfileId);
opaque_id!(ElementId);
opaque_id!(DetectorId);
opaque_id!(RuleId);

/// Portable profile document version one.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Profile {
    /// Schema discriminator used by migration.
    pub schema: u16,
    /// Stable profile identity.
    pub id: ProfileId,
    /// Optimistic-concurrency revision.
    pub revision: u64,
    /// User-facing profile name.
    pub name: String,
    /// Stable external game identifier.
    pub game: String,
    /// Layout reference metadata.
    pub layout: LayoutMetadata,
    /// Configured HUD elements.
    pub elements: Vec<Element>,
    /// Temporal rules consuming element observations.
    pub rules: Vec<EventRule>,
}

impl Profile {
    /// Creates an empty version-one profile.
    #[must_use]
    pub fn new(name: impl Into<String>, game: impl Into<String>, width: u32, height: u32) -> Self {
        Self {
            schema: PROFILE_SCHEMA_VERSION,
            id: ProfileId::new(),
            revision: 0,
            name: name.into(),
            game: game.into(),
            layout: LayoutMetadata {
                reference_width: width,
                reference_height: height,
                ui_scale: None,
                language: None,
            },
            elements: Vec::new(),
            rules: Vec::new(),
        }
    }

    /// Validates all external-contract invariants.
    ///
    /// # Errors
    ///
    /// Returns every invalid field with a GUI-addressable path.
    pub fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = Vec::new();
        if self.schema != PROFILE_SCHEMA_VERSION {
            errors.push(ValidationError::new("schema", "unsupported schema version"));
        }
        validate_identifier("game", &self.game, &mut errors);
        if self.name.trim().is_empty() {
            errors.push(ValidationError::new("name", "must not be empty"));
        }
        if self.layout.reference_width == 0 || self.layout.reference_height == 0 {
            errors.push(ValidationError::new(
                "layout",
                "reference resolution must have non-zero dimensions",
            ));
        }
        for (index, element) in self.elements.iter().enumerate() {
            element.validate(index, &mut errors);
        }
        for (index, rule) in self.rules.iter().enumerate() {
            rule.validate(index, &mut errors);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ValidationErrors(errors))
        }
    }
}

/// Metadata needed to interpret normalized regions across resolutions.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LayoutMetadata {
    pub reference_width: u32,
    pub reference_height: u32,
    pub ui_scale: Option<f32>,
    pub language: Option<String>,
}

/// A normalized rectangle where the origin is the top-left corner.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct NormalizedRegion {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl NormalizedRegion {
    fn validate(self, path: &str, errors: &mut Vec<ValidationError>) {
        let values = [self.x, self.y, self.width, self.height];
        if values.iter().any(|value| !value.is_finite()) {
            errors.push(ValidationError::new(path, "coordinates must be finite"));
        } else if self.x < 0.0 || self.y < 0.0 || self.width <= 0.0 || self.height <= 0.0 {
            errors.push(ValidationError::new(
                path,
                "origin must be non-negative and area must be positive",
            ));
        } else if self.x + self.width > 1.0 || self.y + self.height > 1.0 {
            errors.push(ValidationError::new(
                path,
                "region must remain within [0,1]",
            ));
        }
    }
}

/// One editable HUD element and its detector.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Element {
    pub id: ElementId,
    pub name: String,
    pub enabled: bool,
    pub color: String,
    pub region: NormalizedRegion,
    pub detector: Detector,
}

impl Element {
    fn validate(&self, index: usize, errors: &mut Vec<ValidationError>) {
        let base = format!("elements[{index}]");
        if self.name.trim().is_empty() {
            errors.push(ValidationError::new(
                format!("{base}.name"),
                "must not be empty",
            ));
        }
        self.region.validate(&format!("{base}.region"), errors);
    }
}

/// Deterministic detector configuration.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Detector {
    ColorBar {
        id: DetectorId,
        direction: BarDirection,
        minimum_rgb: [u8; 3],
        maximum_rgb: [u8; 3],
    },
    Template {
        id: DetectorId,
        templates: Vec<PathBuf>,
        threshold: f32,
    },
    RegionChange {
        id: DetectorId,
        threshold: f32,
    },
}

/// Fill direction for a color bar.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BarDirection {
    LeftToRight,
    RightToLeft,
    TopToBottom,
    BottomToTop,
}

/// A first-slice numeric event rule.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EventRule {
    pub id: RuleId,
    pub element_id: ElementId,
    pub event: String,
    pub enter_below: f64,
    pub leave_above: f64,
    pub minimum_confidence: f32,
    pub required_samples: u16,
    pub sample_window: u16,
    pub cooldown_ms: u64,
}

impl EventRule {
    fn validate(&self, index: usize, errors: &mut Vec<ValidationError>) {
        let base = format!("rules[{index}]");
        validate_identifier(&format!("{base}.event"), &self.event, errors);
        if self.leave_above < self.enter_below {
            errors.push(ValidationError::new(
                format!("{base}.leave_above"),
                "must be greater than or equal to enter_below",
            ));
        }
        if !(0.0..=1.0).contains(&self.minimum_confidence) {
            errors.push(ValidationError::new(
                format!("{base}.minimum_confidence"),
                "must be within [0,1]",
            ));
        }
        if self.required_samples == 0 || self.required_samples > self.sample_window {
            errors.push(ValidationError::new(
                format!("{base}.required_samples"),
                "must be non-zero and no greater than sample_window",
            ));
        }
    }
}

fn validate_identifier(path: &str, value: &str, errors: &mut Vec<ValidationError>) {
    let valid = !value.is_empty()
        && value.len() <= 64
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || (byte == b'_' && index > 0)
        });
    if !valid {
        errors.push(ValidationError::new(
            path,
            "must be a lowercase stable identifier using letters, digits, and underscores",
        ));
    }
}

/// One validation failure with a GUI-addressable field path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ValidationError {
    pub path: String,
    pub message: String,
}

impl ValidationError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

/// All validation failures found in one pass.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("profile validation failed with {} error(s)", .0.len())]
pub struct ValidationErrors(pub Vec<ValidationError>);

/// Resolved application locations following XDG defaults.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppPaths {
    pub config: PathBuf,
    pub data: PathBuf,
    pub state: PathBuf,
    pub cache: PathBuf,
    pub runtime: PathBuf,
}

impl AppPaths {
    /// Resolves paths using explicit environment values, with home fallbacks.
    ///
    /// # Errors
    ///
    /// Returns an error when `XDG_RUNTIME_DIR` is unavailable.
    pub fn resolve(home: &Path, environment: &impl Environment) -> Result<Self, PathError> {
        let under = |variable: &str, fallback: &str| {
            environment
                .var(variable)
                .map_or_else(|| home.join(fallback), PathBuf::from)
                .join("yash-app-events")
        };
        let runtime = environment
            .var("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .ok_or(PathError::MissingRuntimeDirectory)?
            .join("yash-app-events");
        Ok(Self {
            config: under("XDG_CONFIG_HOME", ".config"),
            data: under("XDG_DATA_HOME", ".local/share"),
            state: under("XDG_STATE_HOME", ".local/state"),
            cache: under("XDG_CACHE_HOME", ".cache"),
            runtime,
        })
    }

    /// Returns the directory for portable profiles.
    #[must_use]
    pub fn profiles(&self) -> PathBuf {
        self.data.join("profiles")
    }
}

/// Narrow environment boundary supporting deterministic path tests.
pub trait Environment {
    fn var(&self, name: &str) -> Option<String>;
}

/// The current process environment.
#[derive(Clone, Copy, Debug)]
pub struct ProcessEnvironment;

impl Environment for ProcessEnvironment {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

/// XDG path resolution failure.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PathError {
    #[error("XDG_RUNTIME_DIR is required while the daemon is running")]
    MissingRuntimeDirectory,
}

/// Profile persistence failure.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("profile validation failed: {0}")]
    Validation(#[from] ValidationErrors),
    #[error("profile I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("profile JSON failed: {0}")]
    Json(#[from] serde_json::Error),
}

/// Loads, migrates, and validates a profile without modifying its source.
///
/// # Errors
///
/// Returns I/O, JSON, or profile validation errors.
pub fn load_profile(path: &Path) -> Result<Profile, StorageError> {
    let profile: Profile = serde_json::from_reader(BufReader::new(File::open(path)?))?;
    profile.validate()?;
    Ok(profile)
}

/// Validates and atomically replaces a profile document.
///
/// # Errors
///
/// Returns validation, serialization, or durable-write errors.
pub fn save_profile(path: &Path, profile: &Profile) -> Result<(), StorageError> {
    profile.validate()?;
    let bytes = serde_json::to_vec_pretty(profile)?;
    atomic_write(path, &bytes)?;
    Ok(())
}

/// Writes bytes using a same-directory temporary file, flush, sync, and rename.
///
/// # Errors
///
/// Returns an I/O error and leaves an existing destination unchanged.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write_before_rename(path, bytes, || Ok(()))
}

fn atomic_write_before_rename(
    path: &Path,
    bytes: &[u8],
    before_rename: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "output path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("profile"),
        Uuid::new_v4()
    ));
    let result = (|| {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(bytes)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        before_rename()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    struct FakeEnvironment(HashMap<String, String>);

    impl Environment for FakeEnvironment {
        fn var(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    #[test]
    fn schema_round_trip_preserves_semantics() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profile.json");
        let profile = Profile::new("Demo", "demo_game", 1920, 1080);
        save_profile(&path, &profile).unwrap();
        assert_eq!(load_profile(&path).unwrap(), profile);
    }

    #[test]
    fn invalid_region_reports_a_precise_path() {
        let mut profile = Profile::new("Demo", "demo_game", 1920, 1080);
        profile.elements.push(Element {
            id: ElementId::new(),
            name: "Health".into(),
            enabled: true,
            color: "#ff0000".into(),
            region: NormalizedRegion {
                x: 0.9,
                y: 0.0,
                width: 0.2,
                height: 0.1,
            },
            detector: Detector::RegionChange {
                id: DetectorId::new(),
                threshold: 0.2,
            },
        });
        let errors = profile.validate().unwrap_err();
        assert_eq!(errors.0[0].path, "elements[0].region");
    }

    #[test]
    fn interrupted_atomic_write_retains_previous_document() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        atomic_write(&path, b"old").unwrap();
        let error =
            atomic_write_before_rename(&path, b"new", || Err(io::Error::other("injected failure")));
        assert!(error.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn xdg_paths_use_overrides_and_home_fallbacks() {
        let environment = FakeEnvironment(HashMap::from([
            ("XDG_DATA_HOME".into(), "/data".into()),
            ("XDG_RUNTIME_DIR".into(), "/run/user/1000".into()),
        ]));
        let paths = AppPaths::resolve(Path::new("/home/test"), &environment).unwrap();
        assert_eq!(paths.data, Path::new("/data/yash-app-events"));
        assert_eq!(
            paths.config,
            Path::new("/home/test/.config/yash-app-events")
        );
        assert_eq!(paths.runtime, Path::new("/run/user/1000/yash-app-events"));
    }
}
