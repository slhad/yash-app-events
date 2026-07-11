//! Portable profile schemas, validation, migration, and durable local storage.

use std::collections::HashSet;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

mod archive;
mod local;
mod store;
pub use archive::{export_profile, import_profile, ArchiveError, ImportLimits, Manifest};
pub use local::{CaptureBinding, LocalConfig, LocalConfigError, Settings};
pub use store::{ProfileStore, StoreError};

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
        report_duplicate_ids(
            self.elements.iter().map(|element| element.id),
            "elements",
            &mut errors,
        );
        let element_ids: HashSet<_> = self.elements.iter().map(|element| element.id).collect();
        for (index, rule) in self.rules.iter().enumerate() {
            rule.validate(index, &element_ids, &mut errors);
            if !element_ids.contains(&rule.element_id) {
                errors.push(ValidationError::new(
                    format!("rules[{index}].element_id"),
                    "must reference an existing element",
                ));
            }
        }
        report_duplicate_ids(self.rules.iter().map(|rule| rule.id), "rules", &mut errors);
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
        self.detector.validate(&format!("{base}.detector"), errors);
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
        #[serde(default)]
        mask: Option<PathBuf>,
    },
    Template {
        id: DetectorId,
        templates: Vec<PathBuf>,
        #[serde(default)]
        masks: Vec<Option<PathBuf>>,
        threshold: f32,
        #[serde(default)]
        preprocessing: Vec<PreprocessOperation>,
    },
    RegionChange {
        id: DetectorId,
        threshold: f32,
        #[serde(default)]
        preprocessing: Vec<PreprocessOperation>,
    },
    Ocr {
        id: DetectorId,
        language: String,
        page_segmentation_mode: u8,
        #[serde(default)]
        character_whitelist: Option<String>,
        #[serde(default = "default_ocr_change_threshold")]
        change_trigger_threshold: f32,
        #[serde(default = "default_ocr_maximum_interval_ms")]
        maximum_interval_ms: u64,
        #[serde(default)]
        preprocessing: Vec<PreprocessOperation>,
    },
    Classifier {
        id: DetectorId,
        model: PathBuf,
        model_sha256: String,
        labels: Vec<String>,
        input_width: usize,
        input_height: usize,
        #[serde(default)]
        preprocessing: Vec<PreprocessOperation>,
        #[serde(default = "default_classifier_change_threshold")]
        change_trigger_threshold: f32,
        #[serde(default = "default_ocr_maximum_interval_ms")]
        maximum_interval_ms: u64,
    },
}

const fn default_ocr_change_threshold() -> f32 {
    0.02
}

const fn default_ocr_maximum_interval_ms() -> u64 {
    1_000
}

const fn default_classifier_change_threshold() -> f32 {
    0.02
}

/// Explicit deterministic detector preprocessing stored in portable profiles.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PreprocessOperation {
    Resize { width: usize, height: usize },
    Threshold { minimum: u8, maximum: u8 },
    Erode { radius: u8 },
    Dilate { radius: u8 },
    Invert,
}

impl Detector {
    fn assign_new_id(&mut self) {
        match self {
            Self::ColorBar { id, .. }
            | Self::Template { id, .. }
            | Self::RegionChange { id, .. }
            | Self::Ocr { id, .. }
            | Self::Classifier { id, .. } => {
                *id = DetectorId::new();
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn validate(&self, path: &str, errors: &mut Vec<ValidationError>) {
        match self {
            Self::ColorBar {
                minimum_rgb,
                maximum_rgb,
                mask,
                ..
            } => {
                if minimum_rgb
                    .iter()
                    .zip(maximum_rgb)
                    .any(|(minimum, maximum)| minimum > maximum)
                {
                    errors.push(ValidationError::new(
                        path,
                        "minimum RGB channels must not exceed maximum channels",
                    ));
                }
                if let Some(mask) = mask {
                    validate_asset_path(&format!("{path}.mask"), mask, errors);
                }
            }
            Self::Template {
                templates,
                masks,
                threshold,
                preprocessing,
                ..
            } => {
                if templates.is_empty() {
                    errors.push(ValidationError::new(
                        format!("{path}.templates"),
                        "must contain at least one template",
                    ));
                }
                for (index, template) in templates.iter().enumerate() {
                    validate_asset_path(&format!("{path}.templates[{index}]"), template, errors);
                }
                if !masks.is_empty() && masks.len() != templates.len() {
                    errors.push(ValidationError::new(
                        format!("{path}.masks"),
                        "must be empty or align one-for-one with templates",
                    ));
                }
                for (index, mask) in masks.iter().enumerate() {
                    if let Some(mask) = mask {
                        validate_asset_path(&format!("{path}.masks[{index}]"), mask, errors);
                    }
                }
                validate_unit_interval(&format!("{path}.threshold"), *threshold, errors);
                validate_preprocessing(path, preprocessing, errors);
            }
            Self::RegionChange {
                threshold,
                preprocessing,
                ..
            } => {
                validate_unit_interval(&format!("{path}.threshold"), *threshold, errors);
                validate_preprocessing(path, preprocessing, errors);
            }
            Self::Ocr {
                language,
                page_segmentation_mode,
                character_whitelist,
                change_trigger_threshold,
                maximum_interval_ms,
                preprocessing,
                ..
            } => {
                validate_ocr(
                    path,
                    language,
                    *page_segmentation_mode,
                    character_whitelist.as_deref(),
                    *change_trigger_threshold,
                    *maximum_interval_ms,
                    errors,
                );
                validate_preprocessing(path, preprocessing, errors);
            }
            Self::Classifier {
                model,
                model_sha256,
                labels,
                input_width,
                input_height,
                preprocessing,
                change_trigger_threshold,
                maximum_interval_ms,
                ..
            } => {
                validate_classifier(
                    path,
                    model,
                    model_sha256,
                    labels,
                    *input_width,
                    *input_height,
                    *change_trigger_threshold,
                    *maximum_interval_ms,
                    errors,
                );
                validate_preprocessing(path, preprocessing, errors);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_classifier(
    path: &str,
    model: &Path,
    model_sha256: &str,
    labels: &[String],
    input_width: usize,
    input_height: usize,
    change_trigger_threshold: f32,
    maximum_interval_ms: u64,
    errors: &mut Vec<ValidationError>,
) {
    validate_asset_path(&format!("{path}.model"), model, errors);
    if model_sha256.len() != 64 || !model_sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        errors.push(ValidationError::new(
            format!("{path}.model_sha256"),
            "must be a 64-character hexadecimal SHA-256",
        ));
    }
    let unique: HashSet<_> = labels.iter().collect();
    if labels.len() < 2
        || labels.len() > 256
        || unique.len() != labels.len()
        || labels
            .iter()
            .any(|label| label.is_empty() || label.len() > 64)
    {
        errors.push(ValidationError::new(
            format!("{path}.labels"),
            "must contain 2 through 256 unique labels of 1 through 64 bytes",
        ));
    }
    if input_width == 0
        || input_height == 0
        || input_width
            .checked_mul(input_height)
            .is_none_or(|pixels| pixels > 16_777_216)
    {
        errors.push(ValidationError::new(
            format!("{path}.input_width"),
            "input dimensions must contain 1 through 16777216 pixels",
        ));
    }
    validate_unit_interval(
        &format!("{path}.change_trigger_threshold"),
        change_trigger_threshold,
        errors,
    );
    if !(100..=60_000).contains(&maximum_interval_ms) {
        errors.push(ValidationError::new(
            format!("{path}.maximum_interval_ms"),
            "must be within 100 through 60000",
        ));
    }
}

fn validate_ocr(
    path: &str,
    language: &str,
    page_segmentation_mode: u8,
    character_whitelist: Option<&str>,
    change_trigger_threshold: f32,
    maximum_interval_ms: u64,
    errors: &mut Vec<ValidationError>,
) {
    if language.is_empty()
        || language.len() > 32
        || !language
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'+'))
    {
        errors.push(ValidationError::new(
            format!("{path}.language"),
            "must be a 1 through 32 byte Tesseract language identifier",
        ));
    }
    if page_segmentation_mode > 13 {
        errors.push(ValidationError::new(
            format!("{path}.page_segmentation_mode"),
            "must be within 0 through 13",
        ));
    }
    if character_whitelist.is_some_and(|whitelist| whitelist.len() > 256) {
        errors.push(ValidationError::new(
            format!("{path}.character_whitelist"),
            "must not exceed 256 bytes",
        ));
    }
    validate_unit_interval(
        &format!("{path}.change_trigger_threshold"),
        change_trigger_threshold,
        errors,
    );
    if !(100..=60_000).contains(&maximum_interval_ms) {
        errors.push(ValidationError::new(
            format!("{path}.maximum_interval_ms"),
            "must be within 100 through 60000",
        ));
    }
}

fn validate_asset_path(path: &str, asset: &Path, errors: &mut Vec<ValidationError>) {
    if asset.is_absolute()
        || asset
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        errors.push(ValidationError::new(
            path,
            "must be a relative path without parent traversal",
        ));
    }
}

fn validate_preprocessing(
    path: &str,
    operations: &[PreprocessOperation],
    errors: &mut Vec<ValidationError>,
) {
    for (index, operation) in operations.iter().enumerate() {
        let invalid = match operation {
            PreprocessOperation::Resize { width, height } => {
                *width == 0
                    || *height == 0
                    || width
                        .checked_mul(*height)
                        .is_none_or(|pixels| pixels > 16_777_216)
            }
            PreprocessOperation::Threshold { minimum, maximum } => minimum > maximum,
            PreprocessOperation::Erode { radius } | PreprocessOperation::Dilate { radius } => {
                *radius > 8
            }
            PreprocessOperation::Invert => false,
        };
        if invalid {
            errors.push(ValidationError::new(
                format!("{path}.preprocessing[{index}]"),
                "operation parameters are invalid",
            ));
        }
    }
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

/// Predicate applied to an element observation.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RulePredicate {
    #[default]
    NumericBelow,
    Boolean {
        expected: bool,
    },
    TextEquals {
        expected: String,
    },
    TextContains {
        needle: String,
    },
    All {
        conditions: Vec<ObservationCondition>,
    },
    Any {
        conditions: Vec<ObservationCondition>,
    },
}

/// One bounded leaf in an observation composition.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObservationCondition {
    pub element_id: ElementId,
    pub predicate: AtomicRulePredicate,
}

/// Non-recursive predicate used by composition leaves.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AtomicRulePredicate {
    Boolean { expected: bool },
    TextEquals { expected: String },
    TextContains { needle: String },
    NumericBelow { threshold_micros: i32 },
}

fn is_default_rule_predicate(predicate: &RulePredicate) -> bool {
    *predicate == RulePredicate::NumericBelow
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

/// A version-one-compatible event rule with optional post-release typed behavior.
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
    #[serde(default, skip_serializing_if = "is_default_rule_predicate")]
    pub predicate: RulePredicate,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub stable_for_ms: u64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub emit_initial: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_interval_ms: Option<u64>,
}

impl EventRule {
    fn validate(
        &self,
        index: usize,
        element_ids: &HashSet<ElementId>,
        errors: &mut Vec<ValidationError>,
    ) {
        let base = format!("rules[{index}]");
        validate_identifier(&format!("{base}.event"), &self.event, errors);
        if !self.enter_below.is_finite()
            || !self.leave_above.is_finite()
            || self.leave_above < self.enter_below
        {
            errors.push(ValidationError::new(
                format!("{base}.leave_above"),
                "thresholds must be finite and leave_above must be greater than or equal to enter_below",
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
        if self.update_interval_ms == Some(0) {
            errors.push(ValidationError::new(
                format!("{base}.update_interval_ms"),
                "must be greater than zero when enabled",
            ));
        }
        match &self.predicate {
            RulePredicate::NumericBelow | RulePredicate::Boolean { .. } => {}
            RulePredicate::TextEquals { expected } => {
                validate_match_text(&format!("{base}.predicate.expected"), expected, errors);
            }
            RulePredicate::TextContains { needle } => {
                validate_match_text(&format!("{base}.predicate.needle"), needle, errors);
            }
            RulePredicate::All { conditions } | RulePredicate::Any { conditions } => {
                if conditions.is_empty() || conditions.len() > 16 {
                    errors.push(ValidationError::new(
                        format!("{base}.predicate.conditions"),
                        "must contain 1 through 16 observation conditions",
                    ));
                }
                for (condition_index, condition) in conditions.iter().enumerate() {
                    let path = format!("{base}.predicate.conditions[{condition_index}]");
                    if !element_ids.contains(&condition.element_id) {
                        errors.push(ValidationError::new(
                            format!("{path}.element_id"),
                            "must reference an existing element",
                        ));
                    }
                    match &condition.predicate {
                        AtomicRulePredicate::TextEquals { expected } => {
                            validate_match_text(
                                &format!("{path}.predicate.expected"),
                                expected,
                                errors,
                            );
                        }
                        AtomicRulePredicate::TextContains { needle } => {
                            validate_match_text(
                                &format!("{path}.predicate.needle"),
                                needle,
                                errors,
                            );
                        }
                        AtomicRulePredicate::Boolean { .. }
                        | AtomicRulePredicate::NumericBelow { .. } => {}
                    }
                }
            }
        }
    }
}

fn validate_match_text(path: &str, value: &str, errors: &mut Vec<ValidationError>) {
    if value.is_empty() || value.len() > 256 {
        errors.push(ValidationError::new(
            path,
            "must contain 1 through 256 bytes",
        ));
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

fn validate_unit_interval(path: &str, value: f32, errors: &mut Vec<ValidationError>) {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        errors.push(ValidationError::new(
            path,
            "must be finite and within [0,1]",
        ));
    }
}

fn report_duplicate_ids<T: Copy + Eq + std::hash::Hash>(
    ids: impl Iterator<Item = T>,
    path: &str,
    errors: &mut Vec<ValidationError>,
) {
    let mut seen = HashSet::new();
    if ids.into_iter().any(|id| !seen.insert(id)) {
        errors.push(ValidationError::new(path, "contains duplicate stable IDs"));
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
    #[error("unsupported profile schema {0}")]
    UnsupportedSchema(u64),
}

/// Loads, migrates, and validates a profile without modifying its source.
///
/// # Errors
///
/// Returns I/O, JSON, or profile validation errors.
pub fn load_profile(path: &Path) -> Result<Profile, StorageError> {
    let document: serde_json::Value = serde_json::from_reader(BufReader::new(File::open(path)?))?;
    let schema = document
        .get("schema")
        .and_then(serde_json::Value::as_u64)
        .ok_or(StorageError::UnsupportedSchema(0))?;
    let profile: Profile = match schema {
        1 => serde_json::from_value(document)?,
        version => return Err(StorageError::UnsupportedSchema(version)),
    };
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
    fn version_one_golden_fixture_is_a_stable_external_contract() {
        let profile = load_profile(Path::new("tests/fixtures/profile-v1.json")).unwrap();
        assert_eq!(profile.schema, 1);
        assert_eq!(
            profile.id.to_string(),
            "10000000-0000-4000-8000-000000000001"
        );
        assert_eq!(profile.elements.len(), 1);
        assert_eq!(profile.rules[0].event, "critical_health");
        assert_eq!(profile.rules[0].predicate, RulePredicate::NumericBelow);
        let serialized = serde_json::to_value(&profile).unwrap();
        assert!(serialized["rules"][0].get("predicate").is_none());
        assert!(serialized["rules"][0].get("stable_for_ms").is_none());
        assert!(serialized["rules"][0].get("emit_initial").is_none());
        assert!(serialized["rules"][0].get("update_interval_ms").is_none());
    }

    #[test]
    fn composed_rules_are_bounded_and_reference_existing_observations() {
        let mut profile = Profile::new("Demo", "demo_game", 1920, 1080);
        let element_id = ElementId::new();
        profile.elements.push(Element {
            id: element_id,
            name: "Victory".into(),
            enabled: true,
            color: "#ffffff".into(),
            region: NormalizedRegion {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            detector: Detector::RegionChange {
                id: DetectorId::new(),
                threshold: 0.2,
                preprocessing: Vec::new(),
            },
        });
        profile.rules.push(EventRule {
            id: RuleId::new(),
            element_id,
            event: "victory".into(),
            enter_below: 0.2,
            leave_above: 0.3,
            minimum_confidence: 0.0,
            required_samples: 1,
            sample_window: 1,
            cooldown_ms: 0,
            predicate: RulePredicate::All {
                conditions: vec![ObservationCondition {
                    element_id,
                    predicate: AtomicRulePredicate::Boolean { expected: true },
                }],
            },
            stable_for_ms: 250,
            emit_initial: true,
            update_interval_ms: Some(1_000),
        });
        profile.validate().unwrap();
        if let RulePredicate::All { conditions } = &mut profile.rules[0].predicate {
            conditions[0].element_id = ElementId::new();
        }
        let errors = profile.validate().unwrap_err();
        assert!(errors
            .0
            .iter()
            .any(|error| { error.path == "rules[0].predicate.conditions[0].element_id" }));
    }

    #[test]
    fn unsupported_schema_is_rejected_without_rewriting_source() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("future.json");
        fs::write(&path, br#"{"schema":99}"#).unwrap();
        assert!(matches!(
            load_profile(&path),
            Err(StorageError::UnsupportedSchema(99))
        ));
        assert_eq!(fs::read(&path).unwrap(), br#"{"schema":99}"#);
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
                preprocessing: Vec::new(),
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
