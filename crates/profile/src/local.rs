use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{atomic_write, ProfileId};

/// Machine-local daemon settings stored outside portable profiles.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Settings {
    pub schema: u16,
    pub active_profile: Option<ProfileId>,
    pub analysis_fps: u8,
    pub revision_history_limit: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema: 1,
            active_profile: None,
            analysis_fps: 10,
            revision_history_limit: 20,
        }
    }
}

/// Portal restoration data that must never enter portable profile archives.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CaptureBinding {
    pub restore_token: String,
    pub source_label: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct CaptureBindings {
    schema: u16,
    profiles: HashMap<String, CaptureBinding>,
}

/// Atomic storage for settings and capture bindings below the XDG config root.
#[derive(Clone, Debug)]
pub struct LocalConfig {
    root: PathBuf,
}

impl LocalConfig {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Loads settings or returns defaults when no settings file exists.
    ///
    /// # Errors
    ///
    /// Returns malformed TOML, unsupported schema, validation, or I/O errors.
    pub fn load_settings(&self) -> Result<Settings, LocalConfigError> {
        let path = self.root.join("settings.toml");
        if !path.exists() {
            return Ok(Settings::default());
        }
        let settings: Settings = toml::from_str(&fs::read_to_string(path)?)?;
        validate_settings(&settings)?;
        Ok(settings)
    }

    /// Atomically persists validated daemon settings.
    ///
    /// # Errors
    ///
    /// Returns validation, serialization, or durable-write errors.
    pub fn save_settings(&self, settings: &Settings) -> Result<(), LocalConfigError> {
        validate_settings(settings)?;
        save_toml(&self.root.join("settings.toml"), settings)
    }

    /// Loads one machine-local portal binding.
    ///
    /// # Errors
    ///
    /// Returns malformed TOML, unsupported schema, or I/O errors.
    pub fn capture_binding(
        &self,
        id: ProfileId,
    ) -> Result<Option<CaptureBinding>, LocalConfigError> {
        Ok(self.load_bindings()?.profiles.get(&id.to_string()).cloned())
    }

    /// Atomically sets one portal binding outside the portable profile directory.
    ///
    /// # Errors
    ///
    /// Returns validation, serialization, or durable-write errors.
    pub fn set_capture_binding(
        &self,
        id: ProfileId,
        binding: CaptureBinding,
    ) -> Result<(), LocalConfigError> {
        if binding.restore_token.is_empty() || binding.restore_token.len() > 16 * 1024 {
            return Err(LocalConfigError::Invalid("restore token length"));
        }
        let mut bindings = self.load_bindings()?;
        bindings.schema = 1;
        bindings.profiles.insert(id.to_string(), binding);
        save_toml(&self.root.join("capture-bindings.toml"), &bindings)
    }

    /// Removes a capture binding when a profile is permanently deleted.
    ///
    /// # Errors
    ///
    /// Returns serialization or durable-write errors.
    pub fn remove_capture_binding(&self, id: ProfileId) -> Result<(), LocalConfigError> {
        let mut bindings = self.load_bindings()?;
        bindings.profiles.remove(&id.to_string());
        save_toml(&self.root.join("capture-bindings.toml"), &bindings)
    }

    fn load_bindings(&self) -> Result<CaptureBindings, LocalConfigError> {
        let path = self.root.join("capture-bindings.toml");
        if !path.exists() {
            return Ok(CaptureBindings {
                schema: 1,
                profiles: HashMap::new(),
            });
        }
        let bindings: CaptureBindings = toml::from_str(&fs::read_to_string(path)?)?;
        if bindings.schema != 1 {
            return Err(LocalConfigError::UnsupportedSchema(bindings.schema));
        }
        Ok(bindings)
    }
}

fn validate_settings(settings: &Settings) -> Result<(), LocalConfigError> {
    if settings.schema != 1 {
        return Err(LocalConfigError::UnsupportedSchema(settings.schema));
    }
    if !(1..=10).contains(&settings.analysis_fps) {
        return Err(LocalConfigError::Invalid(
            "analysis_fps must be within 1..=10",
        ));
    }
    if settings.revision_history_limit == 0 || settings.revision_history_limit > 1000 {
        return Err(LocalConfigError::Invalid(
            "revision history limit must be within 1..=1000",
        ));
    }
    Ok(())
}

fn save_toml<T: Serialize>(path: &Path, value: &T) -> Result<(), LocalConfigError> {
    atomic_write(path, toml::to_string_pretty(value)?.as_bytes())?;
    Ok(())
}

/// Machine-local configuration failure.
#[derive(Debug, Error)]
pub enum LocalConfigError {
    #[error("local configuration I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("local configuration TOML failed: {0}")]
    TomlDeserialize(#[from] toml::de::Error),
    #[error("local configuration TOML serialization failed: {0}")]
    TomlSerialize(#[from] toml::ser::Error),
    #[error("unsupported local configuration schema {0}")]
    UnsupportedSchema(u16),
    #[error("invalid local configuration: {0}")]
    Invalid(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_and_portal_bindings_round_trip_in_separate_files() {
        let directory = tempfile::tempdir().unwrap();
        let config = LocalConfig::new(directory.path());
        let id = ProfileId::new();
        let settings = Settings {
            active_profile: Some(id),
            analysis_fps: 5,
            ..Settings::default()
        };
        config.save_settings(&settings).unwrap();
        config
            .set_capture_binding(
                id,
                CaptureBinding {
                    restore_token: "secret-portal-token".into(),
                    source_label: Some("Game".into()),
                },
            )
            .unwrap();
        assert_eq!(config.load_settings().unwrap(), settings);
        assert_eq!(
            config.capture_binding(id).unwrap().unwrap().restore_token,
            "secret-portal-token"
        );
        assert!(!fs::read_to_string(directory.path().join("settings.toml"))
            .unwrap()
            .contains("secret-portal-token"));
    }

    #[test]
    fn invalid_analysis_rate_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let config = LocalConfig::new(directory.path());
        let settings = Settings {
            analysis_fps: 11,
            ..Settings::default()
        };
        assert!(matches!(
            config.save_settings(&settings),
            Err(LocalConfigError::Invalid(_))
        ));
    }
}
