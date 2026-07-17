use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use yash_app_events_output::{OutputRoute, OutputRouteError};

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
    #[serde(default)]
    pub auto_capture: bool,
    #[serde(default)]
    pub process_match: Option<String>,
}

/// Machine-local passive evidence collection policy for one profile.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CollectionPolicy {
    pub enabled: bool,
    pub dataset_root: PathBuf,
    pub interval_seconds: u64,
    pub jitter_seconds: u64,
    pub similarity_threshold: f32,
    pub maximum_pending_items: usize,
    pub maximum_bytes: u64,
    #[serde(default)]
    pub novelty_targets: Vec<String>,
}

impl Default for CollectionPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            dataset_root: PathBuf::new(),
            interval_seconds: 70,
            jitter_seconds: 10,
            similarity_threshold: 0.015,
            maximum_pending_items: 1_000,
            maximum_bytes: 2 * 1024 * 1024 * 1024,
            novelty_targets: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
struct CollectionPolicies {
    schema: u16,
    profiles: HashMap<String, CollectionPolicy>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct CaptureBindings {
    schema: u16,
    profiles: HashMap<String, CaptureBinding>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
struct OutputRoutes {
    schema: u16,
    profiles: HashMap<String, Vec<OutputRoute>>,
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

    /// Updates automatic capture policy while retaining the private restore token.
    ///
    /// # Errors
    ///
    /// Returns an error when no capture binding exists, the match is oversized, or
    /// the machine-local configuration cannot be persisted atomically.
    pub fn set_auto_capture(
        &self,
        id: ProfileId,
        enabled: bool,
        process_match: Option<String>,
    ) -> Result<CaptureBinding, LocalConfigError> {
        let process_match = process_match.filter(|value| !value.trim().is_empty());
        if process_match
            .as_ref()
            .is_some_and(|value| value.len() > 256)
        {
            return Err(LocalConfigError::Invalid(
                "process match must not exceed 256 bytes",
            ));
        }
        let mut bindings = self.load_bindings()?;
        let binding =
            bindings
                .profiles
                .get_mut(&id.to_string())
                .ok_or(LocalConfigError::Invalid(
                    "capture source must be selected before enabling automatic capture",
                ))?;
        binding.auto_capture = enabled;
        binding.process_match = process_match;
        let result = binding.clone();
        save_toml(&self.root.join("capture-bindings.toml"), &bindings)?;
        Ok(result)
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

    /// Loads one passive collection policy, or safe disabled defaults.
    ///
    /// # Errors
    ///
    /// Returns malformed TOML, unsupported schema, validation, or I/O errors.
    pub fn collection_policy(&self, id: ProfileId) -> Result<CollectionPolicy, LocalConfigError> {
        let policies = self.load_collection_policies()?;
        let policy = policies
            .profiles
            .get(&id.to_string())
            .cloned()
            .unwrap_or_default();
        validate_collection_policy(&policy)?;
        Ok(policy)
    }

    /// Atomically stores a passive collection policy outside portable profile data.
    ///
    /// # Errors
    ///
    /// Returns validation, serialization, or durable-write errors.
    pub fn set_collection_policy(
        &self,
        id: ProfileId,
        policy: CollectionPolicy,
    ) -> Result<(), LocalConfigError> {
        validate_collection_policy(&policy)?;
        let mut policies = self.load_collection_policies()?;
        policies.schema = 1;
        policies.profiles.insert(id.to_string(), policy);
        save_toml(&self.root.join("collection-policies.toml"), &policies)
    }

    /// Loads every machine-local output route for one profile.
    ///
    /// Command routes deliberately live here instead of portable profile archives,
    /// so importing a profile can never authorize executable content.
    ///
    /// # Errors
    ///
    /// Returns I/O, schema, JSON, or route-validation errors.
    pub fn output_routes(&self, id: ProfileId) -> Result<Vec<OutputRoute>, LocalConfigError> {
        Ok(self
            .load_output_routes()?
            .profiles
            .get(&id.to_string())
            .cloned()
            .unwrap_or_default())
    }

    /// Creates or replaces one validated output route atomically.
    ///
    /// # Errors
    ///
    /// Returns validation, route-limit, schema, serialization, or durable-write errors.
    pub fn set_output_route(
        &self,
        id: ProfileId,
        route: OutputRoute,
    ) -> Result<Vec<OutputRoute>, LocalConfigError> {
        route.validate()?;
        let mut routes = self.load_output_routes()?;
        routes.schema = 1;
        let profile_routes = routes.profiles.entry(id.to_string()).or_default();
        if let Some(existing) = profile_routes
            .iter_mut()
            .find(|existing| existing.id == route.id)
        {
            *existing = route;
        } else {
            if profile_routes.len() >= 64 {
                return Err(LocalConfigError::Invalid(
                    "a profile may have at most 64 output routes",
                ));
            }
            profile_routes.push(route);
        }
        profile_routes.sort_by(|left, right| left.name.cmp(&right.name));
        let result = profile_routes.clone();
        save_json(&self.root.join("output-routes.json"), &routes)?;
        Ok(result)
    }

    /// Removes one output route atomically and returns whether it existed.
    ///
    /// # Errors
    ///
    /// Returns schema, serialization, or durable-write errors.
    pub fn remove_output_route(
        &self,
        id: ProfileId,
        route_id: uuid::Uuid,
    ) -> Result<bool, LocalConfigError> {
        let mut routes = self.load_output_routes()?;
        let profile_routes = routes.profiles.entry(id.to_string()).or_default();
        let previous = profile_routes.len();
        profile_routes.retain(|route| route.id != route_id);
        let removed = previous != profile_routes.len();
        save_json(&self.root.join("output-routes.json"), &routes)?;
        Ok(removed)
    }

    /// Enables or disables a route without changing its reviewed sink definition.
    ///
    /// # Errors
    ///
    /// Returns an error when the route is absent or local configuration cannot be saved.
    pub fn set_output_enabled(
        &self,
        id: ProfileId,
        route_id: uuid::Uuid,
        enabled: bool,
    ) -> Result<Vec<OutputRoute>, LocalConfigError> {
        let mut routes = self.load_output_routes()?;
        let profile_routes = routes.profiles.entry(id.to_string()).or_default();
        let route = profile_routes
            .iter_mut()
            .find(|route| route.id == route_id)
            .ok_or(LocalConfigError::Invalid("output route was not found"))?;
        route.enabled = enabled;
        let result = profile_routes.clone();
        save_json(&self.root.join("output-routes.json"), &routes)?;
        Ok(result)
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

    fn load_collection_policies(&self) -> Result<CollectionPolicies, LocalConfigError> {
        let path = self.root.join("collection-policies.toml");
        if !path.exists() {
            return Ok(CollectionPolicies {
                schema: 1,
                profiles: HashMap::new(),
            });
        }
        let policies: CollectionPolicies = toml::from_str(&fs::read_to_string(path)?)?;
        if policies.schema != 1 {
            return Err(LocalConfigError::UnsupportedSchema(policies.schema));
        }
        for policy in policies.profiles.values() {
            validate_collection_policy(policy)?;
        }
        Ok(policies)
    }

    fn load_output_routes(&self) -> Result<OutputRoutes, LocalConfigError> {
        let path = self.root.join("output-routes.json");
        if !path.exists() {
            return Ok(OutputRoutes {
                schema: 1,
                profiles: HashMap::new(),
            });
        }
        let routes: OutputRoutes = serde_json::from_slice(&fs::read(path)?)?;
        if routes.schema != 1 {
            return Err(LocalConfigError::UnsupportedSchema(routes.schema));
        }
        for profile_routes in routes.profiles.values() {
            if profile_routes.len() > 64 {
                return Err(LocalConfigError::Invalid(
                    "a profile may have at most 64 output routes",
                ));
            }
            for route in profile_routes {
                route.validate()?;
            }
        }
        Ok(routes)
    }
}

fn validate_collection_policy(policy: &CollectionPolicy) -> Result<(), LocalConfigError> {
    if policy.enabled && !policy.dataset_root.is_absolute() {
        return Err(LocalConfigError::Invalid(
            "enabled collection dataset root must be absolute",
        ));
    }
    if !(10..=86_400).contains(&policy.interval_seconds) {
        return Err(LocalConfigError::Invalid(
            "collection interval must be within 10..=86400 seconds",
        ));
    }
    if policy.jitter_seconds >= policy.interval_seconds {
        return Err(LocalConfigError::Invalid(
            "collection jitter must be smaller than its interval",
        ));
    }
    if !policy.similarity_threshold.is_finite()
        || !(0.0..=0.25).contains(&policy.similarity_threshold)
    {
        return Err(LocalConfigError::Invalid(
            "collection similarity threshold must be within 0..=0.25",
        ));
    }
    if policy.maximum_pending_items == 0 || policy.maximum_pending_items > 100_000 {
        return Err(LocalConfigError::Invalid(
            "collection pending-item limit must be within 1..=100000",
        ));
    }
    if !(1024 * 1024..=1024_u64.pow(4)).contains(&policy.maximum_bytes) {
        return Err(LocalConfigError::Invalid(
            "collection byte limit must be within 1 MiB..=1 TiB",
        ));
    }
    if policy.novelty_targets.len() > 256
        || policy
            .novelty_targets
            .iter()
            .any(|target| target.is_empty() || target.len() > 128)
    {
        return Err(LocalConfigError::Invalid(
            "collection novelty targets are invalid",
        ));
    }
    Ok(())
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

fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<(), LocalConfigError> {
    atomic_write(path, &serde_json::to_vec_pretty(value)?)?;
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
    #[error("local configuration JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid output route: {0}")]
    OutputRoute(#[from] OutputRouteError),
    #[error("unsupported local configuration schema {0}")]
    UnsupportedSchema(u16),
    #[error("invalid local configuration: {0}")]
    Invalid(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;
    use yash_app_events_output::{FileMode, OutputFormat, OutputSink, OutputTrigger};

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
                    auto_capture: false,
                    process_match: None,
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

    #[test]
    fn collection_policy_is_separate_validated_and_disabled_by_default() {
        let directory = tempfile::tempdir().unwrap();
        let config = LocalConfig::new(directory.path());
        let id = ProfileId::new();
        assert_eq!(config.collection_policy(id).unwrap().interval_seconds, 70);
        let policy = CollectionPolicy {
            enabled: true,
            dataset_root: directory.path().join("dataset"),
            novelty_targets: vec!["stage".into()],
            ..CollectionPolicy::default()
        };
        config.set_collection_policy(id, policy.clone()).unwrap();
        assert_eq!(config.collection_policy(id).unwrap(), policy);
        assert!(directory.path().join("collection-policies.toml").exists());
    }

    #[test]
    fn output_routes_are_machine_local_atomic_and_replace_by_stable_id() {
        let directory = tempfile::tempdir().unwrap();
        let config = LocalConfig::new(directory.path());
        let profile_id = ProfileId::new();
        let mut route = OutputRoute {
            id: Uuid::new_v4(),
            name: "stage markers".into(),
            enabled: false,
            trigger: OutputTrigger::Event {
                events: vec!["stage_changed".into()],
                states: Vec::new(),
            },
            format: OutputFormat::JsonTemplate {
                template: json!({"stage":"{{event.value}}"}),
            },
            sink: OutputSink::File {
                path: directory.path().join("markers.jsonl"),
                mode: FileMode::Append,
            },
            source_recipe: None,
        };
        assert!(config.output_routes(profile_id).unwrap().is_empty());
        assert_eq!(
            config.set_output_route(profile_id, route.clone()).unwrap(),
            vec![route.clone()]
        );
        route.enabled = true;
        assert_eq!(
            config.set_output_route(profile_id, route.clone()).unwrap(),
            vec![route.clone()]
        );
        assert_eq!(
            config.output_routes(profile_id).unwrap(),
            vec![route.clone()]
        );
        assert!(directory.path().join("output-routes.json").exists());
        assert!(config.remove_output_route(profile_id, route.id).unwrap());
        assert!(config.output_routes(profile_id).unwrap().is_empty());
    }
}
