//! Portable profile schemas, validation, migration, and durable local storage.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use uuid::Uuid;

mod archive;
mod local;
mod store;
pub use archive::{export_profile, import_profile, ArchiveError, ImportLimits, Manifest};
pub use local::{CaptureBinding, CollectionPolicy, LocalConfig, LocalConfigError, Settings};
pub use store::{OutputRecipeEntry, ProfileStore, StoreError};
pub use yash_app_events_output::OutputRoute;

/// Current portable profile schema version.
pub const PROFILE_SCHEMA_VERSION: u16 = 2;

const MAXIMUM_SCENES: usize = 128;
const MAXIMUM_OVERLAYS: usize = 128;
const MAXIMUM_ELEMENTS: usize = 512;
const MAXIMUM_LAYER_ELEMENTS: usize = 256;
const MAXIMUM_INTERACTION_TARGETS: usize = 256;
const MAXIMUM_TOTAL_INTERACTION_TARGETS: usize = 512;
const MAXIMUM_RECOGNITION_CONDITIONS: usize = 32;

/// Shared validated profile resource limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ProfileLimits {
    pub elements: usize,
    pub scenes: usize,
    pub overlays: usize,
    pub elements_per_layer: usize,
    pub interaction_targets_per_layer: usize,
    pub interaction_targets_total: usize,
    pub recognition_conditions: usize,
}

impl Default for ProfileLimits {
    fn default() -> Self {
        Self {
            elements: MAXIMUM_ELEMENTS,
            scenes: MAXIMUM_SCENES,
            overlays: MAXIMUM_OVERLAYS,
            elements_per_layer: MAXIMUM_LAYER_ELEMENTS,
            interaction_targets_per_layer: MAXIMUM_INTERACTION_TARGETS,
            interaction_targets_total: MAXIMUM_TOTAL_INTERACTION_TARGETS,
            recognition_conditions: MAXIMUM_RECOGNITION_CONDITIONS,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CapacityUsage {
    pub used: usize,
    pub maximum: usize,
    pub percent: f64,
    pub warning: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProfileCapacityReport {
    pub limits: ProfileLimits,
    pub elements: CapacityUsage,
    pub scenes: CapacityUsage,
    pub overlays: CapacityUsage,
    pub interaction_targets: CapacityUsage,
    pub largest_layer_element_usage: CapacityUsage,
    pub largest_layer_target_usage: CapacityUsage,
    pub largest_recognition_usage: CapacityUsage,
    pub largest_layer_elements: usize,
    pub largest_layer_targets: usize,
    pub recognition_conditions: usize,
    pub detector_families: BTreeMap<String, usize>,
    pub unreachable_elements: Vec<String>,
    pub disabled_layer_only_elements: Vec<String>,
    pub duplicate_detector_groups: Vec<Vec<String>>,
    pub estimated_cost_by_scene: BTreeMap<String, u64>,
    pub suggestions: Vec<String>,
}

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
opaque_id!(SceneId);
opaque_id!(OverlayId);
opaque_id!(InteractionTargetId);

fn capacity_usage(used: usize, maximum: usize) -> CapacityUsage {
    #[allow(clippy::cast_precision_loss)]
    let percent = used as f64 * 100.0 / maximum as f64;
    let warning = if used >= maximum {
        Some("limit_reached")
    } else if used * 100 >= maximum * 90 {
        Some("ninety_percent")
    } else if used * 100 >= maximum * 80 {
        Some("eighty_percent")
    } else {
        None
    };
    CapacityUsage {
        used,
        maximum,
        percent,
        warning,
    }
}

fn recognition_conditions(expression: &RecognitionExpression) -> &[RecognitionCondition] {
    match expression {
        RecognitionExpression::All { conditions } | RecognitionExpression::Any { conditions } => {
            conditions
        }
    }
}

fn detector_family(detector: &Detector) -> &'static str {
    match detector {
        Detector::ColorBar { .. } => "color_bar",
        Detector::Template { .. } => "template",
        Detector::RegionChange { .. } => "region_change",
        Detector::Ocr { .. } => "ocr",
        Detector::SevenSegment { .. } => "seven_segment",
        Detector::Classifier { .. } => "classifier",
    }
}

fn detector_cost(detector: &Detector) -> u64 {
    match detector {
        Detector::ColorBar { .. } | Detector::RegionChange { .. } => 1,
        Detector::Template { templates, .. } => u64::try_from(templates.len()).unwrap_or(u64::MAX),
        Detector::SevenSegment { .. } => 3,
        Detector::Ocr { .. } | Detector::Classifier { .. } => 20,
    }
}

fn detector_signature(element: &Element) -> String {
    let mut detector = serde_json::to_value(&element.detector).unwrap_or_default();
    if let Some(object) = detector.as_object_mut() {
        object.remove("id");
    }
    serde_json::to_string(&(element.region, detector)).unwrap_or_default()
}

/// Current portable profile document.
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
    /// Values composed from named detector observations.
    #[serde(default)]
    pub derived_observations: Vec<DerivedObservation>,
    /// Temporal rules consuming element observations.
    pub rules: Vec<EventRule>,
    /// Elements evaluated independently of scene selection.
    #[serde(default)]
    pub global_elements: Vec<ElementId>,
    /// Mutually exclusive base screens.
    #[serde(default)]
    pub scenes: Vec<Scene>,
    /// Independently active layers such as dialogs and rewards.
    #[serde(default)]
    pub overlays: Vec<Overlay>,
}

impl Profile {
    /// Creates an empty profile using the current schema.
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
                require_reference_dimensions: false,
            },
            elements: Vec::new(),
            derived_observations: Vec::new(),
            rules: Vec::new(),
            global_elements: Vec::new(),
            scenes: Vec::new(),
            overlays: Vec::new(),
        }
    }

    /// Reports bounded profile usage and read-only consolidation opportunities.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn analyze_capacity(&self) -> ProfileCapacityReport {
        let limits = ProfileLimits::default();
        let all_layers = self
            .scenes
            .iter()
            .map(|scene| {
                (
                    &scene.element_ids,
                    &scene.interaction_targets,
                    scene.enabled,
                )
            })
            .chain(self.overlays.iter().map(|overlay| {
                (
                    &overlay.element_ids,
                    &overlay.interaction_targets,
                    overlay.enabled,
                )
            }))
            .collect::<Vec<_>>();
        let largest_layer_elements = all_layers
            .iter()
            .map(|(elements, _, _)| elements.len())
            .max()
            .unwrap_or(0);
        let largest_layer_targets = all_layers
            .iter()
            .map(|(_, targets, _)| targets.len())
            .max()
            .unwrap_or(0);
        let interaction_targets = all_layers
            .iter()
            .map(|(_, targets, _)| targets.len())
            .sum::<usize>();
        let recognition_condition_count = self
            .scenes
            .iter()
            .map(|scene| recognition_conditions(&scene.recognition).len())
            .chain(
                self.overlays
                    .iter()
                    .map(|overlay| recognition_conditions(&overlay.recognition).len()),
            )
            .sum();
        let largest_recognition_conditions = self
            .scenes
            .iter()
            .map(|scene| recognition_conditions(&scene.recognition).len())
            .chain(
                self.overlays
                    .iter()
                    .map(|overlay| recognition_conditions(&overlay.recognition).len()),
            )
            .max()
            .unwrap_or(0);
        let mut referenced = self.global_elements.iter().copied().collect::<HashSet<_>>();
        let mut enabled_referenced = referenced.clone();
        for scene in &self.scenes {
            referenced.extend(scene.element_ids.iter().copied());
            referenced.extend(
                recognition_conditions(&scene.recognition)
                    .iter()
                    .map(|condition| condition.element_id),
            );
            if scene.enabled {
                enabled_referenced.extend(scene.element_ids.iter().copied());
            }
        }
        for overlay in &self.overlays {
            referenced.extend(overlay.element_ids.iter().copied());
            referenced.extend(
                recognition_conditions(&overlay.recognition)
                    .iter()
                    .map(|condition| condition.element_id),
            );
            if overlay.enabled {
                enabled_referenced.extend(overlay.element_ids.iter().copied());
            }
        }
        let unreachable_elements = self
            .elements
            .iter()
            .filter(|element| !referenced.contains(&element.id))
            .map(|element| element.name.clone())
            .collect::<Vec<_>>();
        let disabled_layer_only_elements = self
            .elements
            .iter()
            .filter(|element| {
                referenced.contains(&element.id) && !enabled_referenced.contains(&element.id)
            })
            .map(|element| element.name.clone())
            .collect::<Vec<_>>();
        let mut families = BTreeMap::new();
        let mut duplicate_candidates: HashMap<String, Vec<String>> = HashMap::new();
        for element in &self.elements {
            *families
                .entry(detector_family(&element.detector).to_owned())
                .or_insert(0) += 1;
            duplicate_candidates
                .entry(detector_signature(element))
                .or_default()
                .push(element.name.clone());
        }
        let mut duplicate_detector_groups = duplicate_candidates
            .into_values()
            .filter(|elements| elements.len() > 1)
            .collect::<Vec<_>>();
        duplicate_detector_groups.sort_by(|left, right| left[0].cmp(&right[0]));
        let elements_by_id = self
            .elements
            .iter()
            .map(|element| (element.id, element))
            .collect::<HashMap<_, _>>();
        let estimated_cost_by_scene = self
            .scenes
            .iter()
            .map(|scene| {
                let ids = self
                    .global_elements
                    .iter()
                    .chain(scene.element_ids.iter())
                    .copied()
                    .collect::<HashSet<_>>();
                let cost = ids
                    .iter()
                    .filter_map(|id| elements_by_id.get(id))
                    .map(|element| detector_cost(&element.detector))
                    .sum();
                (scene.name.clone(), cost)
            })
            .collect();
        let mut suggestions = Vec::new();
        if !unreachable_elements.is_empty() {
            suggestions.push("remove or assign unreachable detector elements".into());
        }
        if !duplicate_detector_groups.is_empty() {
            suggestions.push(
                "review duplicate regions/configurations and consolidate stable references".into(),
            );
        }
        if self.elements.len() * 100 >= limits.elements * 80 {
            suggestions.push(
                "prefer shared anchors, contextual elements, derived observations, and multi-template detectors before adding elements".into(),
            );
        }
        ProfileCapacityReport {
            limits,
            elements: capacity_usage(self.elements.len(), limits.elements),
            scenes: capacity_usage(self.scenes.len(), limits.scenes),
            overlays: capacity_usage(self.overlays.len(), limits.overlays),
            interaction_targets: capacity_usage(
                interaction_targets,
                limits.interaction_targets_total,
            ),
            largest_layer_element_usage: capacity_usage(
                largest_layer_elements,
                limits.elements_per_layer,
            ),
            largest_layer_target_usage: capacity_usage(
                largest_layer_targets,
                limits.interaction_targets_per_layer,
            ),
            largest_recognition_usage: capacity_usage(
                largest_recognition_conditions,
                limits.recognition_conditions,
            ),
            largest_layer_elements,
            largest_layer_targets,
            recognition_conditions: recognition_condition_count,
            detector_families: families,
            unreachable_elements,
            disabled_layer_only_elements,
            duplicate_detector_groups,
            estimated_cost_by_scene,
            suggestions,
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
        if self.elements.len() > MAXIMUM_ELEMENTS {
            errors.push(ValidationError::new(
                "elements",
                format!(
                    "{}/{} detector elements requested; the 512/512 capacity limit is reached; run `profile analyze-capacity` before adding another element",
                    self.elements.len(),
                    MAXIMUM_ELEMENTS,
                ),
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
        let mut element_ids: HashSet<_> = self.elements.iter().map(|element| element.id).collect();
        for (index, derived) in self.derived_observations.iter().enumerate() {
            derived.validate(index, &element_ids, &mut errors);
            if !element_ids.insert(derived.id) {
                errors.push(ValidationError::new(
                    format!("derived_observations[{index}].id"),
                    "must be unique across detector and derived observations",
                ));
            }
        }
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
        self.validate_scene_model(&element_ids, &mut errors);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ValidationErrors(errors))
        }
    }

    #[allow(clippy::too_many_lines)]
    fn validate_scene_model(
        &self,
        element_ids: &HashSet<ElementId>,
        errors: &mut Vec<ValidationError>,
    ) {
        if self.scenes.len() > MAXIMUM_SCENES {
            errors.push(ValidationError::new(
                "scenes",
                "contains more than 128 scenes",
            ));
        }
        if self.overlays.len() > MAXIMUM_OVERLAYS {
            errors.push(ValidationError::new(
                "overlays",
                "contains more than 128 overlays",
            ));
        }
        report_duplicate_ids(self.scenes.iter().map(|scene| scene.id), "scenes", errors);
        report_duplicate_ids(
            self.overlays.iter().map(|overlay| overlay.id),
            "overlays",
            errors,
        );
        validate_element_references(
            "global_elements",
            &self.global_elements,
            element_ids,
            errors,
        );
        let scene_ids = self
            .scenes
            .iter()
            .map(|scene| scene.id)
            .collect::<HashSet<_>>();
        let enabled_detector_ids = self
            .elements
            .iter()
            .filter(|element| element.enabled)
            .map(|element| element.id)
            .collect::<HashSet<_>>();
        for (index, scene) in self.scenes.iter().enumerate() {
            scene.validate(index, element_ids, &scene_ids, errors);
            validate_enabled_recognition(
                &format!("scenes[{index}].recognition"),
                &scene.recognition,
                &enabled_detector_ids,
                errors,
            );
            validate_required_enabled(
                &format!("scenes[{index}].required_element_ids"),
                &scene.required_element_ids,
                &enabled_detector_ids,
                errors,
            );
            for (target_index, target) in scene.interaction_targets.iter().enumerate() {
                if let Some(visibility) = &target.visibility {
                    validate_enabled_recognition(
                        &format!("scenes[{index}].interaction_targets[{target_index}].visibility"),
                        visibility,
                        &enabled_detector_ids,
                        errors,
                    );
                }
            }
        }
        for (index, overlay) in self.overlays.iter().enumerate() {
            overlay.validate(index, element_ids, errors);
            validate_enabled_recognition(
                &format!("overlays[{index}].recognition"),
                &overlay.recognition,
                &enabled_detector_ids,
                errors,
            );
            validate_required_enabled(
                &format!("overlays[{index}].required_element_ids"),
                &overlay.required_element_ids,
                &enabled_detector_ids,
                errors,
            );
            for (target_index, target) in overlay.interaction_targets.iter().enumerate() {
                if let Some(visibility) = &target.visibility {
                    validate_enabled_recognition(
                        &format!(
                            "overlays[{index}].interaction_targets[{target_index}].visibility"
                        ),
                        visibility,
                        &enabled_detector_ids,
                        errors,
                    );
                }
            }
        }
        let targets = self
            .scenes
            .iter()
            .flat_map(|scene| scene.interaction_targets.iter())
            .chain(
                self.overlays
                    .iter()
                    .flat_map(|overlay| overlay.interaction_targets.iter()),
            )
            .map(|target| target.id)
            .collect::<Vec<_>>();
        if targets.len() > MAXIMUM_TOTAL_INTERACTION_TARGETS {
            errors.push(ValidationError::new(
                "interaction_targets",
                "contains more than 512 targets across all scenes and overlays",
            ));
        }
        report_duplicate_ids(targets.into_iter(), "interaction_targets", errors);
    }
}

/// One base game screen selected exclusively from competing candidates.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Scene {
    pub id: SceneId,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub enabled: bool,
    #[serde(default)]
    pub priority: i16,
    pub recognition: RecognitionExpression,
    pub minimum_confidence: f32,
    pub ambiguity_margin: f32,
    pub required_samples: u16,
    pub sample_window: u16,
    #[serde(default)]
    pub element_ids: Vec<ElementId>,
    /// Contextual observations that must be valid before JSON-only handoff.
    #[serde(default)]
    pub required_element_ids: Vec<ElementId>,
    #[serde(default)]
    pub interaction_targets: Vec<InteractionTarget>,
    #[serde(default)]
    pub transition_hints: Vec<SceneId>,
}

impl Scene {
    fn validate(
        &self,
        index: usize,
        element_ids: &HashSet<ElementId>,
        scene_ids: &HashSet<SceneId>,
        errors: &mut Vec<ValidationError>,
    ) {
        let base = format!("scenes[{index}]");
        validate_layer_name(&base, &self.name, errors);
        validate_recognition(
            &format!("{base}.recognition"),
            &self.recognition,
            element_ids,
            errors,
        );
        validate_confidence(
            &format!("{base}.minimum_confidence"),
            self.minimum_confidence,
            errors,
        );
        validate_confidence(
            &format!("{base}.ambiguity_margin"),
            self.ambiguity_margin,
            errors,
        );
        if self.required_samples == 0
            || self.required_samples > self.sample_window
            || self.sample_window > 32
        {
            errors.push(ValidationError::new(
                format!("{base}.required_samples"),
                "must be non-zero, no greater than sample_window, with sample_window at most 32",
            ));
        }
        validate_layer_contents(
            &base,
            &self.element_ids,
            &self.required_element_ids,
            &self.interaction_targets,
            element_ids,
            errors,
        );
        for (hint_index, hint) in self.transition_hints.iter().enumerate() {
            if *hint == self.id || !scene_ids.contains(hint) {
                errors.push(ValidationError::new(
                    format!("{base}.transition_hints[{hint_index}]"),
                    "must reference a different existing scene",
                ));
            }
        }
    }
}

/// One independently recognized layer over a base scene.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Overlay {
    pub id: OverlayId,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub enabled: bool,
    /// When active, do not expose interaction targets owned by the underlying scene.
    #[serde(default)]
    pub blocks_scene_targets: bool,
    #[serde(default)]
    pub priority: i16,
    pub recognition: RecognitionExpression,
    pub minimum_confidence: f32,
    pub required_samples: u16,
    pub sample_window: u16,
    #[serde(default)]
    pub element_ids: Vec<ElementId>,
    /// Contextual observations that must be valid before JSON-only handoff.
    #[serde(default)]
    pub required_element_ids: Vec<ElementId>,
    #[serde(default)]
    pub interaction_targets: Vec<InteractionTarget>,
}

impl Overlay {
    fn validate(
        &self,
        index: usize,
        element_ids: &HashSet<ElementId>,
        errors: &mut Vec<ValidationError>,
    ) {
        let base = format!("overlays[{index}]");
        validate_layer_name(&base, &self.name, errors);
        validate_recognition(
            &format!("{base}.recognition"),
            &self.recognition,
            element_ids,
            errors,
        );
        validate_confidence(
            &format!("{base}.minimum_confidence"),
            self.minimum_confidence,
            errors,
        );
        if self.required_samples == 0
            || self.required_samples > self.sample_window
            || self.sample_window > 32
        {
            errors.push(ValidationError::new(
                format!("{base}.required_samples"),
                "must be non-zero, no greater than sample_window, with sample_window at most 32",
            ));
        }
        validate_layer_contents(
            &base,
            &self.element_ids,
            &self.required_element_ids,
            &self.interaction_targets,
            element_ids,
            errors,
        );
    }
}

/// Bounded non-recursive expression used for recognition and visibility.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RecognitionExpression {
    All {
        conditions: Vec<RecognitionCondition>,
    },
    Any {
        conditions: Vec<RecognitionCondition>,
    },
}

impl Default for RecognitionExpression {
    fn default() -> Self {
        Self::All {
            conditions: Vec::new(),
        }
    }
}

/// One typed observation predicate contributing to scene recognition.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RecognitionCondition {
    pub element_id: ElementId,
    pub predicate: AtomicRulePredicate,
    #[serde(default = "default_condition_weight")]
    pub weight: f32,
}

const fn default_condition_weight() -> f32 {
    1.0
}

/// Declarative, non-executable UI geometry.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InteractionTarget {
    pub id: InteractionTargetId,
    pub name: String,
    pub role: InteractionRole,
    pub region: NormalizedRegion,
    #[serde(default)]
    pub interaction_point: Option<InteractionPoint>,
    #[serde(default)]
    pub visibility: Option<RecognitionExpression>,
    #[serde(default)]
    pub required_for_json: bool,
    #[serde(default)]
    pub caution_class: Option<InteractionCaution>,
    #[serde(default)]
    pub caution: Option<String>,
}

/// Semantic use of a described target.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionRole {
    Action,
    Navigation,
    Dismiss,
    Selection,
}

/// Gameplay risk classification carried as data; it never authorizes input.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionCaution {
    Navigation,
    RoutineAction,
    Consequential,
    NeverAutomatic,
}

/// How a safe point is obtained. Neither variant performs input.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum InteractionPoint {
    Explicit { x: f32, y: f32 },
    Center,
}

fn validate_layer_name(base: &str, name: &str, errors: &mut Vec<ValidationError>) {
    validate_identifier(&format!("{base}.name"), name, errors);
}

fn validate_confidence(path: &str, value: f32, errors: &mut Vec<ValidationError>) {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        errors.push(ValidationError::new(
            path,
            "must be finite and within [0,1]",
        ));
    }
}

fn validate_element_references(
    path: &str,
    references: &[ElementId],
    element_ids: &HashSet<ElementId>,
    errors: &mut Vec<ValidationError>,
) {
    if references.len() > MAXIMUM_LAYER_ELEMENTS {
        errors.push(ValidationError::new(
            path,
            "contains more than 256 element references",
        ));
    }
    let mut seen = HashSet::new();
    for (index, id) in references.iter().enumerate() {
        if !element_ids.contains(id) || !seen.insert(*id) {
            errors.push(ValidationError::new(
                format!("{path}[{index}]"),
                "must reference a unique existing detector element",
            ));
        }
    }
}

fn validate_recognition(
    path: &str,
    expression: &RecognitionExpression,
    element_ids: &HashSet<ElementId>,
    errors: &mut Vec<ValidationError>,
) {
    let conditions = match expression {
        RecognitionExpression::All { conditions } | RecognitionExpression::Any { conditions } => {
            conditions
        }
    };
    if conditions.len() > MAXIMUM_RECOGNITION_CONDITIONS {
        errors.push(ValidationError::new(
            path,
            "contains more than 32 recognition conditions",
        ));
    }
    let mut seen = HashSet::new();
    for (index, condition) in conditions.iter().enumerate() {
        if !element_ids.contains(&condition.element_id) || !seen.insert(condition.element_id) {
            errors.push(ValidationError::new(
                format!("{path}.conditions[{index}].element_id"),
                "must reference a unique existing detector element",
            ));
        }
        if !condition.weight.is_finite() || !(0.0..=100.0).contains(&condition.weight) {
            errors.push(ValidationError::new(
                format!("{path}.conditions[{index}].weight"),
                "must be finite and within [0,100]",
            ));
        }
    }
}

fn validate_enabled_recognition(
    path: &str,
    expression: &RecognitionExpression,
    enabled_detector_ids: &HashSet<ElementId>,
    errors: &mut Vec<ValidationError>,
) {
    let conditions = match expression {
        RecognitionExpression::All { conditions } | RecognitionExpression::Any { conditions } => {
            conditions
        }
    };
    for (index, condition) in conditions.iter().enumerate() {
        if !enabled_detector_ids.contains(&condition.element_id) {
            errors.push(ValidationError::new(
                format!("{path}.conditions[{index}].element_id"),
                "must reference an enabled detector element",
            ));
        }
    }
}

fn validate_required_enabled(
    path: &str,
    references: &[ElementId],
    enabled_detector_ids: &HashSet<ElementId>,
    errors: &mut Vec<ValidationError>,
) {
    for (index, id) in references.iter().enumerate() {
        if !enabled_detector_ids.contains(id) {
            errors.push(ValidationError::new(
                format!("{path}[{index}]"),
                "must reference an enabled detector element",
            ));
        }
    }
}

fn validate_layer_contents(
    base: &str,
    element_references: &[ElementId],
    required_references: &[ElementId],
    targets: &[InteractionTarget],
    element_ids: &HashSet<ElementId>,
    errors: &mut Vec<ValidationError>,
) {
    validate_element_references(
        &format!("{base}.element_ids"),
        element_references,
        element_ids,
        errors,
    );
    validate_element_references(
        &format!("{base}.required_element_ids"),
        required_references,
        element_ids,
        errors,
    );
    for (index, id) in required_references.iter().enumerate() {
        if !element_references.contains(id) {
            errors.push(ValidationError::new(
                format!("{base}.required_element_ids[{index}]"),
                "must also appear in the layer element_ids",
            ));
        }
    }
    if targets.len() > MAXIMUM_INTERACTION_TARGETS {
        errors.push(ValidationError::new(
            format!("{base}.interaction_targets"),
            "contains more than 256 interaction targets",
        ));
    }
    for (index, target) in targets.iter().enumerate() {
        let path = format!("{base}.interaction_targets[{index}]");
        validate_identifier(&format!("{path}.name"), &target.name, errors);
        target.region.validate(&format!("{path}.region"), errors);
        if let Some(InteractionPoint::Explicit { x, y }) = target.interaction_point {
            let inside = x.is_finite()
                && y.is_finite()
                && x >= target.region.x
                && y >= target.region.y
                && x < target.region.x + target.region.width
                && y < target.region.y + target.region.height;
            if !inside {
                errors.push(ValidationError::new(
                    format!("{path}.interaction_point"),
                    "must be finite and inside the target rectangle",
                ));
            }
        }
        if let Some(visibility) = &target.visibility {
            validate_recognition(
                &format!("{path}.visibility"),
                visibility,
                element_ids,
                errors,
            );
        }
        if target
            .caution
            .as_ref()
            .is_some_and(|value| value.len() > 512)
        {
            errors.push(ValidationError::new(
                format!("{path}.caution"),
                "must contain at most 512 bytes",
            ));
        }
    }
}

fn migrated_scene_id(profile_id: ProfileId) -> SceneId {
    let digest =
        Sha256::digest(format!("yash-app-events:v1-default-scene:{profile_id}").as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    SceneId(Uuid::from_bytes(bytes))
}

fn migrate_v1(document: serde_json::Value) -> Result<Profile, serde_json::Error> {
    let mut profile: Profile = serde_json::from_value(document)?;
    profile.schema = PROFILE_SCHEMA_VERSION;
    profile.scenes = vec![Scene {
        id: migrated_scene_id(profile.id),
        name: "default".into(),
        description: "Implicit scene migrated from schema version 1".into(),
        enabled: true,
        priority: 0,
        recognition: RecognitionExpression::default(),
        minimum_confidence: 0.0,
        ambiguity_margin: 0.0,
        required_samples: 1,
        sample_window: 1,
        element_ids: profile.elements.iter().map(|element| element.id).collect(),
        required_element_ids: Vec::new(),
        interaction_targets: Vec::new(),
        transition_hints: Vec::new(),
    }];
    Ok(profile)
}

/// A daemon-composed text observation with stable identity.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DerivedObservation {
    pub id: ElementId,
    pub detector_id: DetectorId,
    pub name: String,
    pub enabled: bool,
    /// Format string containing `{input_name}` placeholders.
    pub format: String,
    pub inputs: Vec<DerivedInput>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DerivedInput {
    pub name: String,
    pub element_id: ElementId,
}

impl DerivedObservation {
    fn validate(
        &self,
        index: usize,
        element_ids: &HashSet<ElementId>,
        errors: &mut Vec<ValidationError>,
    ) {
        let base = format!("derived_observations[{index}]");
        if self.name.trim().is_empty() || self.format.is_empty() || self.format.len() > 512 {
            errors.push(ValidationError::new(
                base.clone(),
                "name and format must be non-empty and format at most 512 bytes",
            ));
        }
        let mut names = HashSet::new();
        if self.inputs.is_empty() || self.inputs.len() > 16 {
            errors.push(ValidationError::new(
                format!("{base}.inputs"),
                "must contain 1 through 16 inputs",
            ));
        }
        for (input_index, input) in self.inputs.iter().enumerate() {
            let placeholder = format!("{{{}}}", input.name);
            if !names.insert(input.name.as_str()) || !self.format.contains(&placeholder) {
                errors.push(ValidationError::new(
                    format!("{base}.inputs[{input_index}].name"),
                    "must be unique and referenced by the format",
                ));
            }
            if !element_ids.contains(&input.element_id) {
                errors.push(ValidationError::new(
                    format!("{base}.inputs[{input_index}].element_id"),
                    "must reference a detector observation",
                ));
            }
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
    /// Reject frames whose dimensions differ from the authored reference size.
    ///
    /// This remains opt-in so existing normalized, aspect-compatible profiles keep
    /// their resolution-independent behavior.
    #[serde(default)]
    pub require_reference_dimensions: bool,
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
        /// Typed text emitted when the optional HUD value is absent.
        #[serde(default)]
        empty_value: Option<String>,
        /// Left-pad non-empty numeric OCR results with zeroes to this width.
        #[serde(default)]
        zero_pad_to: Option<u8>,
        /// Optional bounded retry guidance for transiently occluded OCR fields.
        #[serde(default)]
        retry: Option<OcrRetryHint>,
    },
    SevenSegment {
        id: DetectorId,
        digits: u8,
        #[serde(default)]
        separator_after: Option<u8>,
        threshold: u8,
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

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OcrRetryHint {
    pub after_ms: u32,
    pub maximum_attempts: u8,
    pub expected_format: OcrExpectedFormat,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OcrExpectedFormat {
    HhMmSs,
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
            | Self::SevenSegment { id, .. }
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
                retry,
                zero_pad_to,
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
                if let Some(retry) = retry {
                    if !(100..=10_000).contains(&retry.after_ms) {
                        errors.push(ValidationError::new(
                            format!("{path}.retry.after_ms"),
                            "must be within 100 through 10000",
                        ));
                    }
                    if !(1..=10).contains(&retry.maximum_attempts) {
                        errors.push(ValidationError::new(
                            format!("{path}.retry.maximum_attempts"),
                            "must be within 1 through 10",
                        ));
                    }
                }
                if zero_pad_to.is_some_and(|width| !(1..=16).contains(&width)) {
                    errors.push(ValidationError::new(
                        format!("{path}.zero_pad_to"),
                        "must be between 1 and 16",
                    ));
                }
                validate_preprocessing(path, preprocessing, errors);
            }
            Self::SevenSegment {
                digits,
                separator_after,
                preprocessing,
                ..
            } => {
                if !(1..=8).contains(digits)
                    || separator_after.is_some_and(|position| position == 0 || position >= *digits)
                {
                    errors.push(ValidationError::new(
                        path,
                        "seven-segment digits must be 1 through 8 and separator_after must be between digits",
                    ));
                }
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
    RapidIncrease {
        minimum_delta: u64,
        within_ms: u64,
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
    NumericAbove { threshold_micros: i32 },
    ConfidenceAbove { threshold_micros: i32 },
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
            RulePredicate::RapidIncrease {
                minimum_delta,
                within_ms,
            } => {
                if *minimum_delta == 0 || !(100..=60_000).contains(within_ms) {
                    errors.push(ValidationError::new(format!("{base}.predicate"), "rapid increase requires a positive delta and a 100 through 60000 ms window"));
                }
            }
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
                        | AtomicRulePredicate::NumericBelow { .. }
                        | AtomicRulePredicate::NumericAbove { .. }
                        | AtomicRulePredicate::ConfidenceAbove { .. } => {}
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
        1 => migrate_v1(document)?,
        2 => serde_json::from_value(document)?,
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
        let path = Path::new("tests/fixtures/profile-v1.json");
        let original = fs::read(path).unwrap();
        let profile = load_profile(path).unwrap();
        let repeated = load_profile(path).unwrap();
        assert_eq!(profile.schema, 2);
        assert_eq!(
            profile.id.to_string(),
            "10000000-0000-4000-8000-000000000001"
        );
        assert_eq!(profile.elements.len(), 1);
        assert_eq!(profile.scenes.len(), 1);
        assert_eq!(profile.scenes[0].name, "default");
        assert_eq!(profile.scenes[0].element_ids, vec![profile.elements[0].id]);
        assert_eq!(profile.scenes[0].id, repeated.scenes[0].id);
        assert_eq!(profile.rules[0].event, "critical_health");
        assert_eq!(profile.rules[0].predicate, RulePredicate::NumericBelow);
        let serialized = serde_json::to_value(&profile).unwrap();
        assert!(serialized["rules"][0].get("predicate").is_none());
        assert!(serialized["rules"][0].get("stable_for_ms").is_none());
        assert!(serialized["rules"][0].get("emit_initial").is_none());
        assert!(serialized["rules"][0].get("update_interval_ms").is_none());
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn version_two_golden_fixture_preserves_scene_geometry_contract() {
        let profile = load_profile(Path::new("tests/fixtures/profile-v2.json")).unwrap();
        profile.validate().unwrap();
        assert_eq!(profile.schema, PROFILE_SCHEMA_VERSION);
        assert_eq!(profile.scenes[0].name, "menu");
        assert_eq!(
            profile.scenes[0].required_element_ids,
            profile.scenes[0].element_ids
        );
        let target = &profile.scenes[0].interaction_targets[0];
        assert_eq!(target.name, "play_button");
        assert_eq!(target.interaction_point, Some(InteractionPoint::Center));
        assert!(target.required_for_json);
    }

    #[test]
    fn capacity_analysis_warns_and_finds_read_only_consolidation_candidates() {
        let mut profile = load_profile(Path::new("tests/fixtures/profile-v2.json")).unwrap();
        let template = profile.elements[0].clone();
        while profile.elements.len() < 410 {
            let mut duplicate = template.clone();
            duplicate.id = ElementId::new();
            duplicate.detector = match duplicate.detector {
                Detector::Template {
                    templates,
                    masks,
                    threshold,
                    preprocessing,
                    ..
                } => Detector::Template {
                    id: DetectorId::new(),
                    templates,
                    masks,
                    threshold,
                    preprocessing,
                },
                detector => detector,
            };
            duplicate.name = format!("duplicate-{}", profile.elements.len());
            profile.elements.push(duplicate);
        }
        let report = profile.analyze_capacity();
        assert_eq!(report.elements.used, 410);
        assert_eq!(report.elements.maximum, 512);
        assert_eq!(report.elements.warning, Some("eighty_percent"));
        assert_eq!(report.unreachable_elements.len(), 409);
        assert_eq!(report.duplicate_detector_groups[0].len(), 410);
        assert!(report
            .suggestions
            .iter()
            .any(|suggestion| suggestion.contains("consolidate")));

        while profile.elements.len() <= 512 {
            let mut extra = template.clone();
            extra.id = ElementId::new();
            extra.name = format!("overflow-{}", profile.elements.len());
            profile.elements.push(extra);
        }
        let errors = profile.validate().unwrap_err();
        assert!(errors.0.iter().any(|error| {
            error.path == "elements"
                && error.message.contains("513/512")
                && error.message.contains("analyze-capacity")
        }));
    }

    #[test]
    fn scene_geometry_and_references_are_strictly_validated() {
        let mut profile = Profile::new("Demo", "demo_game", 558, 992);
        let element_id = ElementId::new();
        profile.elements.push(Element {
            id: element_id,
            name: "Anchor".into(),
            enabled: true,
            color: "#fff".into(),
            region: NormalizedRegion {
                x: 0.1,
                y: 0.1,
                width: 0.2,
                height: 0.1,
            },
            detector: Detector::RegionChange {
                id: DetectorId::new(),
                threshold: 0.1,
                preprocessing: Vec::new(),
            },
        });
        profile.scenes.push(Scene {
            id: SceneId::new(),
            name: "gameplay".into(),
            description: String::new(),
            enabled: true,
            priority: 0,
            recognition: RecognitionExpression::All {
                conditions: vec![RecognitionCondition {
                    element_id,
                    predicate: AtomicRulePredicate::Boolean { expected: true },
                    weight: 1.0,
                }],
            },
            minimum_confidence: 0.8,
            ambiguity_margin: 0.1,
            required_samples: 1,
            sample_window: 1,
            element_ids: vec![element_id],
            required_element_ids: vec![element_id],
            interaction_targets: vec![InteractionTarget {
                id: InteractionTargetId::new(),
                name: "attack".into(),
                role: InteractionRole::Action,
                region: NormalizedRegion {
                    x: 0.5,
                    y: 0.8,
                    width: 0.4,
                    height: 0.1,
                },
                interaction_point: Some(InteractionPoint::Explicit { x: 0.95, y: 0.85 }),
                visibility: None,
                required_for_json: true,
                caution_class: None,
                caution: None,
            }],
            transition_hints: Vec::new(),
        });
        let errors = profile.validate().unwrap_err();
        assert!(errors
            .0
            .iter()
            .any(|error| { error.path == "scenes[0].interaction_targets[0].interaction_point" }));
        profile.scenes[0].interaction_targets[0].interaction_point = Some(InteractionPoint::Center);
        profile.validate().unwrap();
        profile.scenes[0].sample_window = 33;
        assert!(profile
            .validate()
            .unwrap_err()
            .0
            .iter()
            .any(|error| { error.path == "scenes[0].required_samples" }));
        profile.scenes[0].sample_window = 1;
        profile.scenes[0].element_ids.clear();
        assert!(profile
            .validate()
            .unwrap_err()
            .0
            .iter()
            .any(|error| { error.path == "scenes[0].required_element_ids[0]" }));
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
