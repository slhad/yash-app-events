//! Latest-frame scheduling, typed observations, temporal rules, and transitions.

pub mod collection;
pub mod suite;

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use yash_app_events_capture::Frame;
use yash_app_events_profile::{
    AtomicRulePredicate, DetectorId, ElementId, NormalizedRegion, ObservationCondition, Overlay,
    OverlayId, RecognitionCondition, RecognitionExpression, RuleId, RulePredicate, Scene, SceneId,
};
use yash_app_events_vision::{DetectionStatus, DetectionValue, Detector};

/// Default detector analysis rate in frames per second.
pub const DEFAULT_ANALYSIS_FPS: u8 = 10;

/// Monotonic analysis-rate gate independent of producer frame rate.
#[derive(Clone, Debug)]
pub struct AnalysisScheduler {
    interval: Duration,
    last_analysis: Option<Duration>,
}

impl AnalysisScheduler {
    /// Creates a scheduler supporting the required 1 through 10 FPS range.
    ///
    /// # Errors
    ///
    /// Rejects rates outside 1 through 10 FPS.
    pub fn new(frames_per_second: u8) -> Result<Self, EngineError> {
        if !(1..=10).contains(&frames_per_second) {
            return Err(EngineError::InvalidAnalysisRate);
        }
        Ok(Self {
            interval: Duration::from_secs_f64(1.0 / f64::from(frames_per_second)),
            last_analysis: None,
        })
    }

    /// Returns true only when this frame timestamp starts a new analysis interval.
    pub fn should_analyze(&mut self, timestamp: Duration) -> bool {
        if self
            .last_analysis
            .is_none_or(|last| timestamp.saturating_sub(last) >= self.interval)
        {
            self.last_analysis = Some(timestamp);
            true
        } else {
            false
        }
    }
}

/// Validated pixel rectangle derived from normalized profile coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PixelRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Converts a normalized region to a clamped, non-empty pixel rectangle.
///
/// # Errors
///
/// Rejects invalid frame dimensions and normalized regions that yield no pixels.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn normalized_to_pixels(
    region: NormalizedRegion,
    frame_width: u32,
    frame_height: u32,
) -> Result<PixelRegion, EngineError> {
    if frame_width == 0 || frame_height == 0 {
        return Err(EngineError::InvalidFrameDimensions);
    }
    if [region.x, region.y, region.width, region.height]
        .iter()
        .any(|value| !value.is_finite())
        || region.x < 0.0
        || region.y < 0.0
        || region.width <= 0.0
        || region.height <= 0.0
        || region.x + region.width > 1.0
        || region.y + region.height > 1.0
    {
        return Err(EngineError::EmptyRegion);
    }
    let left = (f64::from(region.x) * f64::from(frame_width))
        .floor()
        .max(0.0) as u32;
    let top = (f64::from(region.y) * f64::from(frame_height))
        .floor()
        .max(0.0) as u32;
    let right = (f64::from(region.x + region.width) * f64::from(frame_width))
        .ceil()
        .min(f64::from(frame_width)) as u32;
    let bottom = (f64::from(region.y + region.height) * f64::from(frame_height))
        .ceil()
        .min(f64::from(frame_height)) as u32;
    let width = right.saturating_sub(left);
    let height = bottom.saturating_sub(top);
    if width == 0 || height == 0 {
        return Err(EngineError::EmptyRegion);
    }
    Ok(PixelRegion {
        x: left,
        y: top,
        width,
        height,
    })
}

/// Typed detector output; unknown/error never fabricates a negative numeric value.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Observation {
    pub detector_id: DetectorId,
    pub element_id: ElementId,
    pub timestamp_ms: u64,
    pub value: ObservationValue,
    pub confidence: Option<f32>,
    pub status: ObservationStatus,
    pub diagnostic: String,
}

impl Observation {
    #[must_use]
    pub fn monotonic_timestamp(&self) -> Duration {
        Duration::from_millis(self.timestamp_ms)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ObservationValue {
    Number(f64),
    Boolean(bool),
    Text(String),
    None,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationStatus {
    Valid,
    Unknown,
    Error,
}

/// Stable result of evaluating scene and overlay recognition evidence.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SceneResolution {
    pub scene: Option<ResolvedScene>,
    pub overlays: Vec<ResolvedOverlay>,
    pub ambiguous: bool,
    pub candidates: Vec<SceneCandidate>,
    pub overlay_candidates: Vec<OverlayCandidate>,
}

/// Caller-provided scene and overlay preferences for one analysis sample.
///
/// These IDs are a soft prior only.  The resolver still evaluates every enabled
/// candidate, so an incorrect or stale caller hint cannot hide a valid scene or
/// overlay from the result.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SceneResolutionHints {
    pub preferred_scene_ids: Vec<SceneId>,
    pub preferred_overlay_ids: Vec<OverlayId>,
}

/// Selected mutually exclusive base scene.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ResolvedScene {
    pub id: SceneId,
    pub name: String,
    pub confidence: f32,
}

/// Independently active overlay.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ResolvedOverlay {
    pub id: OverlayId,
    pub name: String,
    pub confidence: f32,
}

/// Diagnostic score for an enabled scene candidate.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SceneCandidate {
    pub id: SceneId,
    pub name: String,
    pub confidence: f32,
    pub eligible: bool,
}

/// Diagnostic score for an enabled overlay candidate.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OverlayCandidate {
    pub id: OverlayId,
    pub name: String,
    pub confidence: f32,
    pub eligible: bool,
}

/// Bounded N-of-M scene resolver fed with the latest anchor observations.
#[derive(Clone, Debug)]
pub struct SceneResolver {
    scenes: Vec<Scene>,
    overlays: Vec<Overlay>,
    scene_index: HashMap<SceneId, usize>,
    overlay_index: HashMap<OverlayId, usize>,
    scene_evidence: HashMap<SceneId, VecDeque<bool>>,
    overlay_evidence: HashMap<OverlayId, VecDeque<bool>>,
    active_scene: Option<SceneId>,
}

impl SceneResolver {
    #[must_use]
    pub fn new(scenes: Vec<Scene>, overlays: Vec<Overlay>) -> Self {
        let scene_index = scenes
            .iter()
            .enumerate()
            .map(|(index, scene)| (scene.id, index))
            .collect();
        let overlay_index = overlays
            .iter()
            .enumerate()
            .map(|(index, overlay)| (overlay.id, index))
            .collect();
        Self {
            scenes,
            overlays,
            scene_index,
            overlay_index,
            scene_evidence: HashMap::new(),
            overlay_evidence: HashMap::new(),
            active_scene: None,
        }
    }

    /// Resolves one analysis sample from the latest observations.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn resolve(&mut self, observations: &HashMap<ElementId, Observation>) -> SceneResolution {
        self.resolve_with_hints(observations, None)
    }

    /// Resolves one sample while using caller preferences as a soft ranking prior.
    ///
    /// Every enabled scene and overlay remains in the evaluation set.  Hints only
    /// break equal-confidence ties (before the profile's transition hints), which
    /// preserves whole-profile recovery when a transition is unexpected.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn resolve_with_hints(
        &mut self,
        observations: &HashMap<ElementId, Observation>,
        hints: Option<&SceneResolutionHints>,
    ) -> SceneResolution {
        let preferred_scene_ids: &[SceneId] =
            hints.map_or(&[], |value| value.preferred_scene_ids.as_slice());
        let preferred_overlay_ids: &[OverlayId] =
            hints.map_or(&[], |value| value.preferred_overlay_ids.as_slice());
        let preferred_scene_ranks = (!preferred_scene_ids.is_empty()).then(|| {
            preferred_scene_ids
                .iter()
                .enumerate()
                .map(|(rank, id)| (*id, rank))
                .collect::<HashMap<_, _>>()
        });
        let preferred_overlay_ranks = (!preferred_overlay_ids.is_empty()).then(|| {
            preferred_overlay_ids
                .iter()
                .enumerate()
                .map(|(rank, id)| (*id, rank))
                .collect::<HashMap<_, _>>()
        });
        let hinted = self
            .active_scene
            .and_then(|active| self.scene_index.get(&active))
            .and_then(|index| self.scenes.get(*index))
            .map_or(&[][..], |scene| scene.transition_hints.as_slice());
        let mut candidates = self
            .scenes
            .iter()
            .filter(|scene| scene.enabled)
            .map(|scene| {
                let confidence = recognition_confidence(&scene.recognition, observations);
                let matched = confidence.is_some_and(|value| value >= scene.minimum_confidence);
                let history = self.scene_evidence.entry(scene.id).or_default();
                push_bounded(history, matched, usize::from(scene.sample_window));
                SceneCandidate {
                    id: scene.id,
                    name: scene.name.clone(),
                    confidence: confidence.unwrap_or(0.0),
                    eligible: history.iter().filter(|value| **value).count()
                        >= usize::from(scene.required_samples),
                }
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .confidence
                .total_cmp(&left.confidence)
                .then_with(|| {
                    let left_rank = preferred_scene_ranks
                        .as_ref()
                        .and_then(|ranks| ranks.get(&left.id))
                        .copied()
                        .unwrap_or(usize::MAX);
                    let right_rank = preferred_scene_ranks
                        .as_ref()
                        .and_then(|ranks| ranks.get(&right.id))
                        .copied()
                        .unwrap_or(usize::MAX);
                    left_rank.cmp(&right_rank)
                })
                .then_with(|| hinted.contains(&right.id).cmp(&hinted.contains(&left.id)))
                .then_with(|| {
                    let left_priority = self
                        .scene_index
                        .get(&left.id)
                        .and_then(|index| self.scenes.get(*index))
                        .map_or(0, |scene| scene.priority);
                    let right_priority = self
                        .scene_index
                        .get(&right.id)
                        .and_then(|index| self.scenes.get(*index))
                        .map_or(0, |scene| scene.priority);
                    right_priority.cmp(&left_priority)
                })
                .then_with(|| left.name.cmp(&right.name))
        });

        let mut eligible = candidates.iter().filter(|candidate| candidate.eligible);
        let best = eligible.next();
        let runner_up = eligible.next();
        let ambiguous = best.is_some_and(|best| {
            runner_up.is_some_and(|runner_up| {
                let margin = self
                    .scene_index
                    .get(&best.id)
                    .and_then(|index| self.scenes.get(*index))
                    .map_or(0.0, |scene| scene.ambiguity_margin);
                best.confidence - runner_up.confidence < margin
            })
        });
        if !ambiguous {
            self.active_scene = best.map(|candidate| candidate.id);
        }
        let scene = self.active_scene.and_then(|id| {
            candidates
                .iter()
                .find(|candidate| candidate.id == id && candidate.eligible)
                .map(|candidate| ResolvedScene {
                    id,
                    name: candidate.name.clone(),
                    confidence: candidate.confidence,
                })
        });

        let mut overlay_candidates = self
            .overlays
            .iter()
            .filter(|overlay| overlay.enabled)
            .map(|overlay| {
                let confidence = recognition_confidence(&overlay.recognition, observations);
                let history = self.overlay_evidence.entry(overlay.id).or_default();
                push_bounded(
                    history,
                    confidence.is_some_and(|value| value >= overlay.minimum_confidence),
                    usize::from(overlay.sample_window),
                );
                OverlayCandidate {
                    id: overlay.id,
                    name: overlay.name.clone(),
                    confidence: confidence.unwrap_or(0.0),
                    eligible: history.iter().filter(|value| **value).count()
                        >= usize::from(overlay.required_samples),
                }
            })
            .collect::<Vec<_>>();
        overlay_candidates.sort_by(|left, right| {
            let left_rank = preferred_overlay_ranks
                .as_ref()
                .and_then(|ranks| ranks.get(&left.id))
                .copied()
                .unwrap_or(usize::MAX);
            let right_rank = preferred_overlay_ranks
                .as_ref()
                .and_then(|ranks| ranks.get(&right.id))
                .copied()
                .unwrap_or(usize::MAX);
            left_rank
                .cmp(&right_rank)
                .then_with(|| left.name.cmp(&right.name))
        });
        let mut highest_priority_by_target: HashMap<&str, i16> = HashMap::new();
        for candidate in overlay_candidates
            .iter()
            .filter(|candidate| candidate.eligible)
        {
            let Some(index) = self.overlay_index.get(&candidate.id) else {
                continue;
            };
            let Some(overlay) = self.overlays.get(*index) else {
                continue;
            };
            for target in &overlay.interaction_targets {
                highest_priority_by_target
                    .entry(target.name.as_str())
                    .and_modify(|priority| *priority = (*priority).max(overlay.priority))
                    .or_insert(overlay.priority);
            }
        }
        let overlays = overlay_candidates
            .iter()
            .filter(|candidate| candidate.eligible)
            // A lower-priority layer with the same named interaction target
            // is a fallback/duplicate for the active higher-priority layer.
            // Keep independent overlays, but do not expose two owners for
            // one click target (for example a generic reward popup beside a
            // route-specific reward result).
            .filter(|candidate| {
                let Some(index) = self.overlay_index.get(&candidate.id) else {
                    return false;
                };
                let Some(current) = self.overlays.get(*index) else {
                    return false;
                };
                !current.interaction_targets.iter().any(|target| {
                    highest_priority_by_target
                        .get(target.name.as_str())
                        .is_some_and(|priority| *priority > current.priority)
                })
            })
            .map(|candidate| ResolvedOverlay {
                id: candidate.id,
                name: candidate.name.clone(),
                confidence: candidate.confidence,
            })
            .collect();

        SceneResolution {
            scene,
            overlays,
            ambiguous,
            candidates,
            overlay_candidates,
        }
    }
}

fn push_bounded(history: &mut VecDeque<bool>, value: bool, capacity: usize) {
    history.push_back(value);
    while history.len() > capacity {
        history.pop_front();
    }
}

/// Evaluates one bounded recognition expression against the latest valid observations.
#[must_use]
pub fn recognition_confidence<S: std::hash::BuildHasher>(
    expression: &RecognitionExpression,
    observations: &HashMap<ElementId, Observation, S>,
) -> Option<f32> {
    let (conditions, require_all) = match expression {
        RecognitionExpression::All { conditions } => (conditions, true),
        RecognitionExpression::Any { conditions } => (conditions, false),
    };
    if conditions.is_empty() {
        return require_all.then_some(1.0);
    }
    let mut total_weight = 0.0_f32;
    let mut matched_weight = 0.0_f32;
    for condition in conditions {
        total_weight += condition.weight;
        if let Some((matched, confidence, weight)) = condition_confidence(condition, observations) {
            if matched {
                matched_weight += confidence * weight;
            }
        } else if require_all {
            return None;
        }
    }
    if !require_all && matched_weight == 0.0 {
        return Some(0.0);
    }
    Some((matched_weight / total_weight).clamp(0.0, 1.0))
}

/// Evaluates a recognition expression as a three-valued boolean result.
///
/// `recognition_confidence` intentionally reports partial evidence for an
/// `All` expression so scene ranking can distinguish close candidates. Target
/// visibility is different: one failed condition must hide the target, while
/// a missing condition must leave it unresolved. Keep that stricter semantics
/// separate from the confidence score used for ranking.
#[must_use]
pub fn recognition_matches<S: std::hash::BuildHasher>(
    expression: &RecognitionExpression,
    observations: &HashMap<ElementId, Observation, S>,
) -> Option<bool> {
    let (conditions, require_all) = match expression {
        RecognitionExpression::All { conditions } => (conditions, true),
        RecognitionExpression::Any { conditions } => (conditions, false),
    };
    if conditions.is_empty() {
        return Some(require_all);
    }

    let mut unknown = false;
    for condition in conditions {
        match condition_confidence(condition, observations).map(|result| result.0) {
            Some(true) if !require_all => return Some(true),
            Some(false) if require_all => return Some(false),
            Some(_) => {}
            None => unknown = true,
        }
    }
    if unknown {
        None
    } else {
        Some(require_all)
    }
}

fn condition_confidence<S: std::hash::BuildHasher>(
    condition: &RecognitionCondition,
    observations: &HashMap<ElementId, Observation, S>,
) -> Option<(bool, f32, f32)> {
    let observation = observations.get(&condition.element_id)?;
    if observation.status != ObservationStatus::Valid {
        return None;
    }
    let matched = match condition.predicate {
        AtomicRulePredicate::ConfidenceAbove { threshold_micros } => {
            f64::from(observation.confidence.unwrap_or(1.0))
                > f64::from(threshold_micros) / 1_000_000.0
        }
        _ => atomic_matches(&condition.predicate, &observation.value)?,
    };
    Some((
        matched,
        observation.confidence.unwrap_or(1.0),
        condition.weight,
    ))
}

/// Typed predicate used by the post-release temporal-rule language.
#[derive(Clone, Debug, PartialEq)]
pub enum ValuePredicate {
    Boolean { expected: bool },
    TextEquals { expected: String },
    TextContains { needle: String },
}

/// Shared temporal behavior for typed predicates.
#[derive(Clone, Debug)]
pub struct TemporalRuleConfig {
    pub id: RuleId,
    pub event: String,
    pub predicate: ValuePredicate,
    pub minimum_confidence: f32,
    pub required_samples: usize,
    pub sample_window: usize,
    pub stable_for: Duration,
    pub cooldown: Duration,
    pub emit_initial: bool,
    pub update_interval: Option<Duration>,
}

/// A typed temporal rule supporting boolean/text matching, stable duration,
/// N-of-M evidence, cooldown, initial transitions, and bounded updates.
#[derive(Clone, Debug)]
pub struct TemporalRule {
    config: TemporalRuleConfig,
    state: Option<bool>,
    evidence: VecDeque<bool>,
    candidate: Option<(bool, Duration)>,
    last_transition: Option<Duration>,
    last_update: Option<Duration>,
}

/// Bounded conjunction/disjunction over the latest valid element observations.
#[derive(Clone, Debug)]
pub struct CompositeRule {
    conditions: Vec<ObservationCondition>,
    require_all: bool,
    latest: HashMap<ElementId, Observation>,
    temporal: TemporalRule,
}

#[derive(Clone, Debug)]
pub struct CompositeRuleConfig {
    pub id: RuleId,
    pub event: String,
    pub predicate: RulePredicate,
    pub minimum_confidence: f32,
    pub required_samples: usize,
    pub sample_window: usize,
    pub stable_for: Duration,
    pub cooldown: Duration,
    pub emit_initial: bool,
    pub update_interval: Option<Duration>,
}

impl CompositeRule {
    /// Constructs a bounded non-recursive composition.
    ///
    /// # Errors
    ///
    /// Rejects non-composite predicates and condition counts outside 1 through 16.
    pub fn new(config: CompositeRuleConfig) -> Result<Self, EngineError> {
        let CompositeRuleConfig {
            id,
            event,
            predicate,
            minimum_confidence,
            required_samples,
            sample_window,
            stable_for,
            cooldown,
            emit_initial,
            update_interval,
        } = config;
        let (conditions, require_all) = match predicate {
            RulePredicate::All { conditions } => (conditions, true),
            RulePredicate::Any { conditions } => (conditions, false),
            _ => return Err(EngineError::InvalidRule),
        };
        if conditions.is_empty() || conditions.len() > 16 {
            return Err(EngineError::InvalidRule);
        }
        let temporal = TemporalRule::new(TemporalRuleConfig {
            id,
            event,
            predicate: ValuePredicate::Boolean { expected: true },
            minimum_confidence,
            required_samples,
            sample_window,
            stable_for,
            cooldown,
            emit_initial,
            update_interval,
        })?;
        Ok(Self {
            conditions,
            require_all,
            latest: HashMap::new(),
            temporal,
        })
    }

    /// Updates one element's latest observation and evaluates once every leaf is known.
    pub fn observe(&mut self, observation: &Observation) -> Option<Transition> {
        if !self
            .conditions
            .iter()
            .any(|condition| condition.element_id == observation.element_id)
        {
            return None;
        }
        if observation.status != ObservationStatus::Valid {
            self.latest.remove(&observation.element_id);
            return None;
        }
        self.latest
            .insert(observation.element_id, observation.clone());
        let mut matched = self.require_all;
        let mut confidence = 1.0_f32;
        for condition in &self.conditions {
            let observation = self.latest.get(&condition.element_id)?;
            let condition_matched = atomic_matches(&condition.predicate, &observation.value)?;
            if self.require_all {
                matched &= condition_matched;
            } else {
                matched |= condition_matched;
            }
            confidence = confidence.min(observation.confidence.unwrap_or(1.0));
        }
        self.temporal
            .observe_boolean(observation, confidence, matched)
    }

    #[must_use]
    pub const fn active(&self) -> Option<bool> {
        self.temporal.active()
    }
}

fn atomic_matches(predicate: &AtomicRulePredicate, value: &ObservationValue) -> Option<bool> {
    match (predicate, value) {
        (AtomicRulePredicate::Boolean { expected }, ObservationValue::Boolean(value)) => {
            Some(value == expected)
        }
        (AtomicRulePredicate::Boolean { expected }, ObservationValue::Number(value)) => {
            numeric_boolean(*value).map(|value| value == *expected)
        }
        (AtomicRulePredicate::TextEquals { expected }, ObservationValue::Text(value)) => {
            Some(value == expected)
        }
        (AtomicRulePredicate::TextContains { needle }, ObservationValue::Text(value)) => {
            Some(value.contains(needle))
        }
        (
            AtomicRulePredicate::NumericBelow { threshold_micros },
            ObservationValue::Number(value),
        ) => Some(*value < f64::from(*threshold_micros) / 1_000_000.0),
        (
            AtomicRulePredicate::NumericAbove { threshold_micros },
            ObservationValue::Number(value),
        ) => Some(*value > f64::from(*threshold_micros) / 1_000_000.0),
        _ => None,
    }
}

impl TemporalRule {
    /// Constructs a validated typed temporal rule.
    ///
    /// # Errors
    ///
    /// Rejects invalid confidence, sampling, empty text, or a zero update interval.
    pub fn new(config: TemporalRuleConfig) -> Result<Self, EngineError> {
        let text_valid = match &config.predicate {
            ValuePredicate::Boolean { .. } => true,
            ValuePredicate::TextEquals { expected } => !expected.is_empty(),
            ValuePredicate::TextContains { needle } => !needle.is_empty(),
        };
        if !(0.0..=1.0).contains(&config.minimum_confidence)
            || config.required_samples == 0
            || config.required_samples > config.sample_window
            || config.update_interval == Some(Duration::ZERO)
            || !text_valid
        {
            return Err(EngineError::InvalidRule);
        }
        let sample_window = config.sample_window;
        Ok(Self {
            config,
            state: None,
            evidence: VecDeque::with_capacity(sample_window),
            candidate: None,
            last_transition: None,
            last_update: None,
        })
    }

    /// Consumes one typed observation and emits at most one meaningful transition.
    pub fn observe(&mut self, observation: &Observation) -> Option<Transition> {
        if observation.status != ObservationStatus::Valid {
            return None;
        }
        let confidence = observation.confidence.unwrap_or(1.0);
        if confidence < self.config.minimum_confidence {
            return None;
        }
        let matched = match (&self.config.predicate, &observation.value) {
            (ValuePredicate::Boolean { expected }, ObservationValue::Boolean(value)) => {
                value == expected
            }
            (ValuePredicate::Boolean { expected }, ObservationValue::Number(value)) => {
                let value = numeric_boolean(*value)?;
                value == *expected
            }
            (ValuePredicate::TextEquals { expected }, ObservationValue::Text(value)) => {
                value == expected
            }
            (ValuePredicate::TextContains { needle }, ObservationValue::Text(value)) => {
                value.contains(needle)
            }
            _ => return None,
        };
        self.observe_match(
            observation.timestamp_ms,
            observation.monotonic_timestamp(),
            numeric_transition_value(&observation.value),
            confidence,
            matched,
        )
    }

    /// Evaluates a pre-computed boolean without constructing a synthetic observation.
    fn observe_boolean(
        &mut self,
        observation: &Observation,
        confidence: f32,
        matched: bool,
    ) -> Option<Transition> {
        if confidence < self.config.minimum_confidence {
            return None;
        }
        self.observe_match(
            observation.timestamp_ms,
            observation.monotonic_timestamp(),
            f64::from(u8::from(matched)),
            confidence,
            matched,
        )
    }

    fn observe_match(
        &mut self,
        timestamp_ms: u64,
        now: Duration,
        value: f64,
        confidence: f32,
        matched: bool,
    ) -> Option<Transition> {
        self.evidence.push_back(matched);
        while self.evidence.len() > self.config.sample_window {
            self.evidence.pop_front();
        }
        let positive =
            self.evidence.iter().filter(|&&sample| sample).count() >= self.config.required_samples;
        let negative =
            self.evidence.iter().filter(|&&sample| !sample).count() >= self.config.required_samples;
        let desired = if positive && self.state != Some(true) {
            Some(true)
        } else if negative && self.state != Some(false) {
            Some(false)
        } else {
            self.state
        };
        if desired != self.state {
            let desired = desired?;
            let since = match self.candidate {
                Some((candidate, since)) if candidate == desired => since,
                _ => {
                    self.candidate = Some((desired, now));
                    now
                }
            };
            if now.saturating_sub(since) < self.config.stable_for {
                return None;
            }
            let previous = self.state;
            if previous.is_some()
                && self
                    .last_transition
                    .is_some_and(|last| now.saturating_sub(last) < self.config.cooldown)
            {
                return None;
            }
            self.state = Some(desired);
            self.candidate = None;
            self.last_update = Some(now);
            if previous.is_none() && !self.config.emit_initial {
                return None;
            }
            self.last_transition = Some(now);
            return Some(self.transition(timestamp_ms, value, confidence, desired));
        }
        self.candidate = None;
        if self.state == Some(true)
            && self.config.update_interval.is_some_and(|interval| {
                self.last_update
                    .is_some_and(|last| now.saturating_sub(last) >= interval)
            })
        {
            self.last_update = Some(now);
            return Some(self.transition_with_state(
                timestamp_ms,
                value,
                confidence,
                TransitionState::Updated,
            ));
        }
        None
    }

    fn transition(
        &self,
        timestamp_ms: u64,
        value: f64,
        confidence: f32,
        active: bool,
    ) -> Transition {
        let state = if active {
            TransitionState::Entered
        } else {
            TransitionState::Left
        };
        self.transition_with_state(timestamp_ms, value, confidence, state)
    }

    fn transition_with_state(
        &self,
        timestamp_ms: u64,
        value: f64,
        confidence: f32,
        state: TransitionState,
    ) -> Transition {
        Transition {
            rule_id: self.config.id,
            event: self.config.event.clone(),
            timestamp_ms,
            state,
            value,
            confidence,
        }
    }

    #[must_use]
    pub const fn active(&self) -> Option<bool> {
        self.state
    }
}

fn numeric_boolean(value: f64) -> Option<bool> {
    if value.abs() <= f64::EPSILON {
        Some(false)
    } else if (value - 1.0).abs() <= f64::EPSILON {
        Some(true)
    } else {
        None
    }
}

fn numeric_transition_value(value: &ObservationValue) -> f64 {
    match value {
        ObservationValue::Number(value) => *value,
        ObservationValue::Boolean(value) => f64::from(u8::from(*value)),
        ObservationValue::Text(_) | ObservationValue::None => 0.0,
    }
}

/// Numeric temporal rule supporting confidence, N-of-M, hysteresis, and cooldown.
#[derive(Clone, Debug)]
pub struct NumericRule {
    pub id: RuleId,
    pub event: String,
    pub enter_below: f64,
    pub leave_above: f64,
    pub minimum_confidence: f32,
    pub required_samples: usize,
    pub sample_window: usize,
    pub cooldown: Duration,
    pub stable_for: Duration,
    pub emit_initial: bool,
    pub update_interval: Option<Duration>,
    state: Option<bool>,
    evidence: VecDeque<bool>,
    candidate: Option<(bool, Duration)>,
    last_transition: Option<Duration>,
    last_update: Option<Duration>,
}

/// Serializable-independent runtime configuration for a numeric temporal rule.
#[derive(Clone, Debug)]
pub struct NumericRuleConfig {
    pub id: RuleId,
    pub event: String,
    pub enter_below: f64,
    pub leave_above: f64,
    pub minimum_confidence: f32,
    pub required_samples: usize,
    pub sample_window: usize,
    pub cooldown: Duration,
    pub stable_for: Duration,
    pub emit_initial: bool,
    pub update_interval: Option<Duration>,
}

impl NumericRule {
    /// Constructs a validated first-slice temporal rule.
    ///
    /// # Errors
    ///
    /// Rejects invalid thresholds, confidence, and N-of-M configuration.
    pub fn new(config: NumericRuleConfig) -> Result<Self, EngineError> {
        let NumericRuleConfig {
            id,
            event,
            enter_below,
            leave_above,
            minimum_confidence,
            required_samples,
            sample_window,
            cooldown,
            stable_for,
            emit_initial,
            update_interval,
        } = config;
        if !enter_below.is_finite()
            || !leave_above.is_finite()
            || leave_above < enter_below
            || !(0.0..=1.0).contains(&minimum_confidence)
            || required_samples == 0
            || required_samples > sample_window
            || update_interval == Some(Duration::ZERO)
        {
            return Err(EngineError::InvalidRule);
        }
        Ok(Self {
            id,
            event,
            enter_below,
            leave_above,
            minimum_confidence,
            required_samples,
            sample_window,
            cooldown,
            stable_for,
            emit_initial,
            update_interval,
            state: None,
            evidence: VecDeque::with_capacity(sample_window),
            candidate: None,
            last_transition: None,
            last_update: None,
        })
    }

    /// Consumes one observation and emits only meaningful state transitions.
    pub fn observe(&mut self, observation: &Observation) -> Option<Transition> {
        let (ObservationStatus::Valid, ObservationValue::Number(value), Some(confidence)) = (
            observation.status,
            &observation.value,
            observation.confidence,
        ) else {
            return None;
        };
        if !value.is_finite() || confidence < self.minimum_confidence {
            return None;
        }
        let desired = if self.state.unwrap_or(false) {
            *value <= self.leave_above
        } else {
            *value < self.enter_below
        };
        self.evidence.push_back(desired);
        while self.evidence.len() > self.sample_window {
            self.evidence.pop_front();
        }
        let positive =
            self.evidence.iter().filter(|&&sample| sample).count() >= self.required_samples;
        let negative =
            self.evidence.iter().filter(|&&sample| !sample).count() >= self.required_samples;
        let next = if positive && self.state != Some(true) {
            Some(true)
        } else if negative && self.state != Some(false) {
            Some(false)
        } else {
            self.state
        };
        let timestamp = observation.monotonic_timestamp();
        let previous = self.state;
        if next == previous {
            self.candidate = None;
            if self.state == Some(true)
                && self.update_interval.is_some_and(|interval| {
                    self.last_update
                        .is_some_and(|last| timestamp.saturating_sub(last) >= interval)
                })
            {
                self.last_update = Some(timestamp);
                return Some(Transition {
                    rule_id: self.id,
                    event: self.event.clone(),
                    timestamp_ms: observation.timestamp_ms,
                    state: TransitionState::Updated,
                    value: *value,
                    confidence,
                });
            }
            return None;
        }
        let next = next?;
        let since = match self.candidate {
            Some((candidate, since)) if candidate == next => since,
            _ => {
                self.candidate = Some((next, timestamp));
                timestamp
            }
        };
        if timestamp.saturating_sub(since) < self.stable_for {
            return None;
        }
        if previous.is_some()
            && self
                .last_transition
                .is_some_and(|last| timestamp.saturating_sub(last) < self.cooldown)
        {
            return None;
        }
        self.state = Some(next);
        self.candidate = None;
        self.last_update = Some(timestamp);
        if previous.is_none() && !self.emit_initial {
            return None;
        }
        self.last_transition = Some(timestamp);
        Some(Transition {
            rule_id: self.id,
            event: self.event.clone(),
            timestamp_ms: observation.timestamp_ms,
            state: if next {
                TransitionState::Entered
            } else {
                TransitionState::Left
            },
            value: *value,
            confidence,
        })
    }

    #[must_use]
    pub const fn active(&self) -> Option<bool> {
        self.state
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Transition {
    pub rule_id: RuleId,
    pub event: String,
    pub timestamp_ms: u64,
    pub state: TransitionState,
    pub value: f64,
    pub confidence: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionState {
    Entered,
    Updated,
    Left,
}

/// Versioned, redistributable synthetic replay fixture.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ReplayManifest {
    pub schema: u32,
    pub profile_id: yash_app_events_profile::ProfileId,
    pub element_id: ElementId,
    /// Detector-specific synthetic fixture values, sampled every 100 ms.
    #[serde(default)]
    pub values: Vec<u8>,
    /// Optional profile-relative PNG frames, sampled every 100 ms.
    #[serde(default)]
    pub image_frames: Vec<std::path::PathBuf>,
    pub expected_events: Vec<ExpectedEvent>,
    #[serde(default)]
    pub regression: ReplayRegression,
}

/// An expected transition annotation and its matching tolerance.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExpectedEvent {
    pub event: String,
    pub state: TransitionState,
    pub timestamp_ms: u64,
    #[serde(default = "default_event_tolerance_ms")]
    pub tolerance_ms: u64,
}

const fn default_event_tolerance_ms() -> u64 {
    100
}

/// Optional acceptance thresholds used by CLI/CI regression checks.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ReplayRegression {
    pub minimum_precision: f64,
    pub minimum_recall: f64,
    pub maximum_mean_latency_ms: Option<f64>,
}

impl Default for ReplayRegression {
    fn default() -> Self {
        Self {
            minimum_precision: 1.0,
            minimum_recall: 1.0,
            maximum_mean_latency_ms: None,
        }
    }
}

/// Event-level replay evaluation result.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ReplayMetrics {
    pub expected: usize,
    pub observed: usize,
    pub matched: usize,
    pub duplicates: usize,
    pub misses: usize,
    pub precision: f64,
    pub recall: f64,
    pub mean_latency_ms: Option<f64>,
    pub passed: bool,
}

/// Matches observed transitions to annotations deterministically in expected order.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn evaluate_replay(
    expected: &[ExpectedEvent],
    observed: &[Transition],
    regression: &ReplayRegression,
) -> ReplayMetrics {
    let mut used = vec![false; observed.len()];
    let mut latencies = Vec::new();
    for annotation in expected {
        let candidate = observed.iter().enumerate().find(|(index, transition)| {
            !used[*index]
                && transition.event == annotation.event
                && transition.state == annotation.state
                && transition.timestamp_ms.abs_diff(annotation.timestamp_ms)
                    <= annotation.tolerance_ms
        });
        if let Some((index, transition)) = candidate {
            used[index] = true;
            latencies.push(transition.timestamp_ms.abs_diff(annotation.timestamp_ms) as f64);
        }
    }
    let matched = latencies.len();
    let duplicates = observed.len().saturating_sub(matched);
    let misses = expected.len().saturating_sub(matched);
    let precision = ratio(matched, observed.len());
    let recall = ratio(matched, expected.len());
    let mean_latency_ms =
        (!latencies.is_empty()).then(|| latencies.iter().sum::<f64>() / latencies.len() as f64);
    let latency_passes = regression
        .maximum_mean_latency_ms
        .is_none_or(|maximum| mean_latency_ms.is_some_and(|latency| latency <= maximum));
    ReplayMetrics {
        expected: expected.len(),
        observed: observed.len(),
        matched,
        duplicates,
        misses,
        precision,
        recall,
        mean_latency_ms,
        passed: precision >= regression.minimum_precision
            && recall >= regression.minimum_recall
            && latency_passes,
    }
}

#[allow(clippy::cast_precision_loss)]
fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum EngineError {
    #[error("analysis FPS must be within 1 through 10")]
    InvalidAnalysisRate,
    #[error("frame dimensions must be non-zero")]
    InvalidFrameDimensions,
    #[error("normalized region maps to no pixels")]
    EmptyRegion,
    #[error("temporal rule configuration is invalid")]
    InvalidRule,
}

/// Runtime boundary for rules consuming typed observations.
pub trait ObservationRule: std::fmt::Debug {
    fn observe(&mut self, observation: &Observation) -> Option<Transition>;
}

impl ObservationRule for NumericRule {
    fn observe(&mut self, observation: &Observation) -> Option<Transition> {
        Self::observe(self, observation)
    }
}

impl ObservationRule for TemporalRule {
    fn observe(&mut self, observation: &Observation) -> Option<Transition> {
        Self::observe(self, observation)
    }
}

impl ObservationRule for CompositeRule {
    fn observe(&mut self, observation: &Observation) -> Option<Transition> {
        Self::observe(self, observation)
    }
}

/// Detector-only processing rule used when a shared rule coordinator consumes results.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopRule;

impl ObservationRule for NoopRule {
    fn observe(&mut self, _observation: &Observation) -> Option<Transition> {
        None
    }
}

/// One analyzed frame's observation and optional meaningful transition.
#[derive(Clone, Debug)]
pub struct ProcessedFrame {
    pub observation: Observation,
    pub transition: Option<Transition>,
}

/// Identical detector/rule path used by replay and live capture frames.
#[derive(Debug)]
pub struct FrameProcessor<D: Detector, R: ObservationRule = NumericRule> {
    scheduler: AnalysisScheduler,
    detector: D,
    region: NormalizedRegion,
    detector_id: DetectorId,
    element_id: ElementId,
    rule: R,
}

impl<D: Detector, R: ObservationRule> FrameProcessor<D, R> {
    #[must_use]
    pub fn new(
        scheduler: AnalysisScheduler,
        detector: D,
        region: NormalizedRegion,
        detector_id: DetectorId,
        element_id: ElementId,
        rule: R,
    ) -> Self {
        Self {
            scheduler,
            detector,
            region,
            detector_id,
            element_id,
            rule,
        }
    }

    /// Processes an eligible timestamped frame through detector then temporal rule.
    pub fn process(&mut self, frame: &Frame) -> Option<ProcessedFrame> {
        if !self.scheduler.should_analyze(frame.timestamp) {
            return None;
        }
        let detection = self.detector.detect(frame, self.region);
        let observation = Observation {
            detector_id: self.detector_id,
            element_id: self.element_id,
            timestamp_ms: u64::try_from(frame.timestamp.as_millis()).unwrap_or(u64::MAX),
            value: detection
                .value
                .map_or(ObservationValue::None, |value| match value {
                    DetectionValue::Number(value) => ObservationValue::Number(value),
                    DetectionValue::Boolean(value) => ObservationValue::Boolean(value),
                    DetectionValue::Text(value) => ObservationValue::Text(value),
                }),
            confidence: detection.confidence,
            status: match detection.status {
                DetectionStatus::Valid => ObservationStatus::Valid,
                DetectionStatus::Unknown => ObservationStatus::Unknown,
                DetectionStatus::Error => ObservationStatus::Error,
            },
            diagnostic: detection.diagnostic,
        };
        let transition = self.rule.observe(&observation);
        Some(ProcessedFrame {
            observation,
            transition,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::*;
    use yash_app_events_capture::{Frame, FrameLayout, PixelFormat, ReplaySource};
    use yash_app_events_profile::{
        DetectorId, ElementId, InteractionRole, InteractionTarget, InteractionTargetId,
        NormalizedRegion, Overlay, OverlayId, RecognitionCondition, RecognitionExpression, RuleId,
        Scene, SceneId,
    };
    use yash_app_events_vision::{ColorBarConfig, ColorBarDetector};

    fn observation(timestamp_ms: u64, value: f64, confidence: f32) -> Observation {
        Observation {
            detector_id: DetectorId::new(),
            element_id: ElementId::new(),
            timestamp_ms,
            value: ObservationValue::Number(value),
            confidence: Some(confidence),
            status: ObservationStatus::Valid,
            diagnostic: String::new(),
        }
    }

    fn typed_observation(timestamp_ms: u64, value: ObservationValue) -> Observation {
        Observation {
            detector_id: DetectorId::new(),
            element_id: ElementId::new(),
            timestamp_ms,
            value,
            confidence: Some(0.9),
            status: ObservationStatus::Valid,
            diagnostic: String::new(),
        }
    }

    fn recognized_scene(
        name: &str,
        element_id: ElementId,
        required_samples: u16,
        sample_window: u16,
    ) -> Scene {
        Scene {
            id: SceneId::new(),
            name: name.into(),
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
            required_samples,
            sample_window,
            element_ids: Vec::new(),
            required_element_ids: Vec::new(),
            interaction_targets: Vec::new(),
            transition_hints: Vec::new(),
        }
    }

    #[test]
    fn scene_resolver_applies_n_of_m_and_keeps_scene_during_ambiguity() {
        let first_element = ElementId::new();
        let second_element = ElementId::new();
        let mut first = recognized_scene("gameplay", first_element, 2, 3);
        let second = recognized_scene("menu", second_element, 1, 1);
        let first_id = first.id;
        first.transition_hints.push(second.id);
        let second_id = second.id;
        let mut resolver = SceneResolver::new(vec![first, second], Vec::new());
        let mut observations = HashMap::new();
        observations.insert(
            first_element,
            Observation {
                detector_id: DetectorId::new(),
                element_id: first_element,
                timestamp_ms: 0,
                value: ObservationValue::Boolean(true),
                confidence: Some(0.95),
                status: ObservationStatus::Valid,
                diagnostic: String::new(),
            },
        );
        assert!(resolver.resolve(&observations).scene.is_none());
        let stable = resolver.resolve(&observations);
        assert_eq!(stable.scene.as_ref().map(|scene| scene.id), Some(first_id));

        observations.insert(
            second_element,
            Observation {
                detector_id: DetectorId::new(),
                element_id: second_element,
                timestamp_ms: 1,
                value: ObservationValue::Boolean(true),
                confidence: Some(0.95),
                status: ObservationStatus::Valid,
                diagnostic: String::new(),
            },
        );
        let ambiguous = resolver.resolve(&observations);
        assert!(ambiguous.ambiguous);
        assert_eq!(ambiguous.candidates[0].id, second_id);
        assert_eq!(
            ambiguous.scene.as_ref().map(|scene| scene.id),
            Some(first_id)
        );
    }

    #[test]
    fn scene_resolver_uses_external_preferences_without_pruning_candidates() {
        let element_id = ElementId::new();
        let first = recognized_scene("first", element_id, 1, 1);
        let mut second = first.clone();
        second.id = SceneId::new();
        second.name = "second".into();
        let second_id = second.id;
        let mut resolver = SceneResolver::new(vec![first, second], Vec::new());
        let observations = HashMap::from([(
            element_id,
            Observation {
                detector_id: DetectorId::new(),
                element_id,
                timestamp_ms: 0,
                value: ObservationValue::Boolean(true),
                confidence: Some(0.95),
                status: ObservationStatus::Valid,
                diagnostic: String::new(),
            },
        )]);

        let hints = SceneResolutionHints {
            preferred_scene_ids: vec![second_id],
            preferred_overlay_ids: Vec::new(),
        };
        let resolution = resolver.resolve_with_hints(&observations, Some(&hints));

        assert_eq!(resolution.candidates.len(), 2);
        assert_eq!(resolution.candidates[0].id, second_id);
        assert!(resolution.ambiguous);
        assert!(resolution.scene.is_none());
    }

    #[test]
    fn scene_resolver_tracks_overlays_independently() {
        let element_id = ElementId::new();
        let overlay_id = OverlayId::new();
        let overlay = Overlay {
            id: overlay_id,
            name: "dialog".into(),
            description: String::new(),
            enabled: true,
            blocks_scene_targets: true,
            priority: 0,
            recognition: RecognitionExpression::Any {
                conditions: vec![RecognitionCondition {
                    element_id,
                    predicate: AtomicRulePredicate::TextContains {
                        needle: "confirm".into(),
                    },
                    weight: 1.0,
                }],
            },
            minimum_confidence: 0.7,
            required_samples: 1,
            sample_window: 2,
            element_ids: Vec::new(),
            required_element_ids: Vec::new(),
            interaction_targets: Vec::new(),
        };
        let mut resolver = SceneResolver::new(Vec::new(), vec![overlay]);
        let mut observations = HashMap::from([(
            element_id,
            Observation {
                detector_id: DetectorId::new(),
                element_id,
                timestamp_ms: 0,
                value: ObservationValue::Text("confirm purchase".into()),
                confidence: Some(0.9),
                status: ObservationStatus::Valid,
                diagnostic: String::new(),
            },
        )]);
        let resolution = resolver.resolve(&observations);
        assert!(resolution.scene.is_none());
        assert_eq!(resolution.overlays[0].id, overlay_id);
        observations.get_mut(&element_id).unwrap().status = ObservationStatus::Unknown;
        assert_eq!(resolver.resolve(&observations).overlays.len(), 1);
        assert!(resolver.resolve(&observations).overlays.is_empty());
    }

    #[test]
    fn scene_resolver_suppresses_lower_priority_duplicate_target_overlay() {
        let element_id = ElementId::new();
        let duplicate_target = || InteractionTarget {
            id: InteractionTargetId::new(),
            name: "dismiss_reward".into(),
            role: InteractionRole::Dismiss,
            region: NormalizedRegion {
                x: 0.25,
                y: 0.25,
                width: 0.5,
                height: 0.5,
            },
            interaction_point: None,
            visibility: None,
            required_for_json: true,
            caution_class: None,
            caution: None,
        };
        let make_overlay = |name: &str, priority: i16| Overlay {
            id: OverlayId::new(),
            name: name.into(),
            description: String::new(),
            enabled: true,
            blocks_scene_targets: true,
            priority,
            recognition: RecognitionExpression::All {
                conditions: vec![RecognitionCondition {
                    element_id,
                    predicate: AtomicRulePredicate::Boolean { expected: true },
                    weight: 1.0,
                }],
            },
            minimum_confidence: 0.7,
            required_samples: 1,
            sample_window: 1,
            element_ids: Vec::new(),
            required_element_ids: Vec::new(),
            interaction_targets: vec![duplicate_target()],
        };
        let fallback = make_overlay("generic_reward", 10);
        let specific = make_overlay("specific_reward", 20);
        let specific_id = specific.id;
        let mut resolver = SceneResolver::new(Vec::new(), vec![fallback, specific]);
        let observations = HashMap::from([(
            element_id,
            Observation {
                detector_id: DetectorId::new(),
                element_id,
                timestamp_ms: 0,
                value: ObservationValue::Boolean(true),
                confidence: Some(0.95),
                status: ObservationStatus::Valid,
                diagnostic: String::new(),
            },
        )]);

        let resolution = resolver.resolve(&observations);
        assert_eq!(resolution.overlays.len(), 1);
        assert_eq!(resolution.overlays[0].id, specific_id);
        assert!(resolution
            .overlay_candidates
            .iter()
            .all(|candidate| candidate.eligible));
    }

    #[test]
    fn visibility_evidence_distinguishes_missing_false_and_visible() {
        let element_id = ElementId::new();
        let expression = RecognitionExpression::All {
            conditions: vec![RecognitionCondition {
                element_id,
                predicate: AtomicRulePredicate::Boolean { expected: true },
                weight: 1.0,
            }],
        };
        let mut observations = HashMap::new();
        assert_eq!(recognition_confidence(&expression, &observations), None);
        observations.insert(
            element_id,
            Observation {
                detector_id: DetectorId::new(),
                element_id,
                timestamp_ms: 0,
                value: ObservationValue::Boolean(false),
                confidence: Some(0.9),
                status: ObservationStatus::Valid,
                diagnostic: String::new(),
            },
        );
        assert_eq!(
            recognition_confidence(&expression, &observations),
            Some(0.0)
        );
        observations.get_mut(&element_id).unwrap().value = ObservationValue::Boolean(true);
        assert_eq!(
            recognition_confidence(&expression, &observations),
            Some(0.9)
        );
    }

    #[test]
    fn removed_evidence_cannot_recognize_a_scene_or_reveal_a_target() {
        let mut profile: yash_app_events_profile::Profile =
            serde_json::from_str(include_str!("../../profile/tests/fixtures/profile-v2.json"))
                .unwrap();
        let element_id = profile.elements[0].id;
        let scene = &mut profile.scenes[0];
        scene.required_samples = 1;
        scene.interaction_targets[0].visibility = Some(scene.recognition.clone());
        let mut observed = typed_observation(0, ObservationValue::Boolean(true));
        observed.element_id = element_id;
        let observations = HashMap::from([(element_id, observed)]);
        assert!(SceneResolver::new(profile.scenes.clone(), Vec::new())
            .resolve(&observations)
            .scene
            .is_some());
        yash_app_events_profile::ProfileStore::remove_element(&mut profile, element_id).unwrap();
        profile.validate().unwrap();
        let empty = HashMap::new();
        assert!(SceneResolver::new(profile.scenes.clone(), Vec::new())
            .resolve(&empty)
            .scene
            .is_none());
        assert_eq!(
            recognition_matches(
                profile.scenes[0].interaction_targets[0]
                    .visibility
                    .as_ref()
                    .unwrap(),
                &empty
            ),
            Some(false)
        );
    }

    #[test]
    fn recognition_matches_keeps_all_expressions_strict() {
        let first_id = ElementId::new();
        let second_id = ElementId::new();
        let expression = RecognitionExpression::All {
            conditions: vec![
                RecognitionCondition {
                    element_id: first_id,
                    predicate: AtomicRulePredicate::Boolean { expected: true },
                    weight: 1.0,
                },
                RecognitionCondition {
                    element_id: second_id,
                    predicate: AtomicRulePredicate::Boolean { expected: true },
                    weight: 1.0,
                },
            ],
        };
        let mut observations = HashMap::new();
        observations.insert(
            first_id,
            Observation {
                detector_id: DetectorId::new(),
                element_id: first_id,
                timestamp_ms: 0,
                value: ObservationValue::Boolean(true),
                confidence: Some(0.9),
                status: ObservationStatus::Valid,
                diagnostic: String::new(),
            },
        );
        assert_eq!(recognition_matches(&expression, &observations), None);

        observations.insert(
            second_id,
            Observation {
                detector_id: DetectorId::new(),
                element_id: second_id,
                timestamp_ms: 1,
                value: ObservationValue::Boolean(false),
                confidence: Some(0.9),
                status: ObservationStatus::Valid,
                diagnostic: String::new(),
            },
        );
        assert_eq!(recognition_matches(&expression, &observations), Some(false));

        observations.get_mut(&second_id).unwrap().value = ObservationValue::Boolean(true);
        assert_eq!(recognition_matches(&expression, &observations), Some(true));
    }

    fn typed_rule(predicate: ValuePredicate) -> TemporalRule {
        TemporalRule::new(TemporalRuleConfig {
            id: RuleId::new(),
            event: "typed_event".into(),
            predicate,
            minimum_confidence: 0.8,
            required_samples: 1,
            sample_window: 1,
            stable_for: Duration::ZERO,
            cooldown: Duration::ZERO,
            emit_initial: false,
            update_interval: None,
        })
        .unwrap()
    }

    #[test]
    fn boolean_rule_emits_appearance_and_disappearance() {
        let mut rule = typed_rule(ValuePredicate::Boolean { expected: true });
        assert!(rule
            .observe(&typed_observation(0, ObservationValue::Boolean(false)))
            .is_none());
        assert_eq!(
            rule.observe(&typed_observation(100, ObservationValue::Boolean(true)))
                .unwrap()
                .state,
            TransitionState::Entered
        );
        assert_eq!(
            rule.observe(&typed_observation(200, ObservationValue::Boolean(false)))
                .unwrap()
                .state,
            TransitionState::Left
        );
        let mut numeric_presence = typed_rule(ValuePredicate::Boolean { expected: true });
        assert!(numeric_presence
            .observe(&observation(0, 0.0, 0.9))
            .is_none());
        assert_eq!(
            numeric_presence
                .observe(&observation(100, 1.0, 0.9))
                .unwrap()
                .state,
            TransitionState::Entered
        );
    }

    #[test]
    fn text_rules_support_equality_and_contains() {
        let mut equals = typed_rule(ValuePredicate::TextEquals {
            expected: "victory".into(),
        });
        assert!(equals
            .observe(&typed_observation(0, ObservationValue::Text("menu".into())))
            .is_none());
        assert_eq!(
            equals
                .observe(&typed_observation(
                    100,
                    ObservationValue::Text("victory".into())
                ))
                .unwrap()
                .state,
            TransitionState::Entered
        );
        let mut contains = typed_rule(ValuePredicate::TextContains {
            needle: "level".into(),
        });
        assert!(contains
            .observe(&typed_observation(
                0,
                ObservationValue::Text("main menu".into())
            ))
            .is_none());
        assert_eq!(
            contains
                .observe(&typed_observation(
                    100,
                    ObservationValue::Text("level complete".into())
                ))
                .unwrap()
                .state,
            TransitionState::Entered
        );
    }

    #[test]
    fn numeric_above_predicate_is_strict_and_typed() {
        let predicate = AtomicRulePredicate::NumericAbove {
            threshold_micros: 750_000,
        };
        assert_eq!(
            atomic_matches(&predicate, &ObservationValue::Number(0.9)),
            Some(true)
        );
        assert_eq!(
            atomic_matches(&predicate, &ObservationValue::Number(0.75)),
            Some(false)
        );
        assert_eq!(
            atomic_matches(&predicate, &ObservationValue::Number(0.2)),
            Some(false)
        );
        assert_eq!(
            atomic_matches(&predicate, &ObservationValue::Text("0.9".into())),
            None
        );
    }

    #[test]
    fn confidence_above_predicate_uses_detector_confidence() {
        let element_id = ElementId::new();
        let condition = RecognitionCondition {
            element_id,
            predicate: AtomicRulePredicate::ConfidenceAbove {
                threshold_micros: 850_000,
            },
            weight: 1.0,
        };
        let mut observations = HashMap::new();
        let mut high = observation(0, 0.0, 0.9);
        high.element_id = element_id;
        observations.insert(element_id, high);
        assert_eq!(
            condition_confidence(&condition, &observations).map(|result| result.0),
            Some(true)
        );

        let mut low = observation(0, 1.0, 0.8);
        low.element_id = element_id;
        observations.insert(element_id, low);
        assert_eq!(
            condition_confidence(&condition, &observations).map(|result| result.0),
            Some(false)
        );
    }

    #[test]
    fn stable_duration_initial_and_rate_limited_updates_are_explicit() {
        let mut rule = TemporalRule::new(TemporalRuleConfig {
            id: RuleId::new(),
            event: "visible".into(),
            predicate: ValuePredicate::Boolean { expected: true },
            minimum_confidence: 0.0,
            required_samples: 1,
            sample_window: 1,
            stable_for: Duration::from_millis(200),
            cooldown: Duration::ZERO,
            emit_initial: true,
            update_interval: Some(Duration::from_millis(300)),
        })
        .unwrap();
        assert!(rule
            .observe(&typed_observation(0, ObservationValue::Boolean(true)))
            .is_none());
        assert!(rule
            .observe(&typed_observation(199, ObservationValue::Boolean(true)))
            .is_none());
        assert_eq!(
            rule.observe(&typed_observation(200, ObservationValue::Boolean(true)))
                .unwrap()
                .state,
            TransitionState::Entered
        );
        assert!(rule
            .observe(&typed_observation(499, ObservationValue::Boolean(true)))
            .is_none());
        assert_eq!(
            rule.observe(&typed_observation(500, ObservationValue::Boolean(true)))
                .unwrap()
                .state,
            TransitionState::Updated
        );
    }

    #[test]
    fn conjunction_and_disjunction_use_bounded_latest_observations() {
        let first = ElementId::new();
        let second = ElementId::new();
        let conditions = vec![
            ObservationCondition {
                element_id: first,
                predicate: AtomicRulePredicate::Boolean { expected: true },
            },
            ObservationCondition {
                element_id: second,
                predicate: AtomicRulePredicate::TextContains {
                    needle: "victory".into(),
                },
            },
        ];
        let build = |predicate| {
            CompositeRule::new(CompositeRuleConfig {
                id: RuleId::new(),
                event: "combined".into(),
                predicate,
                minimum_confidence: 0.0,
                required_samples: 1,
                sample_window: 1,
                stable_for: Duration::ZERO,
                cooldown: Duration::ZERO,
                emit_initial: false,
                update_interval: None,
            })
            .unwrap()
        };
        let with_element = |timestamp_ms, element_id, value| Observation {
            element_id,
            ..typed_observation(timestamp_ms, value)
        };
        let mut all = build(RulePredicate::All {
            conditions: conditions.clone(),
        });
        assert!(all
            .observe(&with_element(0, first, ObservationValue::Boolean(true)))
            .is_none());
        assert!(all
            .observe(&with_element(
                100,
                second,
                ObservationValue::Text("menu".into())
            ))
            .is_none());
        assert_eq!(all.active(), Some(false));
        assert_eq!(
            all.observe(&with_element(
                200,
                second,
                ObservationValue::Text("victory screen".into())
            ))
            .unwrap()
            .state,
            TransitionState::Entered
        );

        let mut any = build(RulePredicate::Any { conditions });
        assert!(any
            .observe(&with_element(0, first, ObservationValue::Boolean(false)))
            .is_none());
        assert!(any
            .observe(&with_element(
                100,
                second,
                ObservationValue::Text("menu".into())
            ))
            .is_none());
        assert_eq!(any.active(), Some(false));
        assert_eq!(
            any.observe(&with_element(200, first, ObservationValue::Boolean(true)))
                .unwrap()
                .state,
            TransitionState::Entered
        );
    }

    #[test]
    fn sixty_fps_input_is_throttled_to_ten_analyses() {
        let mut scheduler = AnalysisScheduler::new(10).unwrap();
        let analyzed = (0..60)
            .filter(|frame| {
                scheduler.should_analyze(Duration::from_nanos(*frame * 1_000_000_000 / 60))
            })
            .count();
        assert_eq!(analyzed, 10);
    }

    #[test]
    fn health_rule_emits_exactly_entered_then_left() {
        let mut rule = NumericRule::new(NumericRuleConfig {
            id: RuleId::new(),
            event: "critical_health".into(),
            enter_below: 0.2,
            leave_above: 0.3,
            minimum_confidence: 0.8,
            required_samples: 2,
            sample_window: 3,
            cooldown: Duration::from_millis(200),
            stable_for: Duration::ZERO,
            emit_initial: false,
            update_interval: None,
        })
        .unwrap();
        let values = [0.8, 0.8, 0.19, 0.18, 0.17, 0.25, 0.31, 0.35];
        let transitions: Vec<_> = values
            .into_iter()
            .enumerate()
            .filter_map(|(index, value)| rule.observe(&observation(index as u64 * 100, value, 0.9)))
            .collect();
        assert_eq!(transitions.len(), 2);
        assert_eq!(transitions[0].state, TransitionState::Entered);
        assert_eq!(transitions[1].state, TransitionState::Left);
    }

    #[test]
    fn unknown_and_low_confidence_do_not_fabricate_negative_evidence() {
        let mut rule = NumericRule::new(NumericRuleConfig {
            id: RuleId::new(),
            event: "critical".into(),
            enter_below: 0.2,
            leave_above: 0.3,
            minimum_confidence: 0.8,
            required_samples: 1,
            sample_window: 1,
            cooldown: Duration::ZERO,
            stable_for: Duration::ZERO,
            emit_initial: false,
            update_interval: None,
        })
        .unwrap();
        assert!(rule.observe(&observation(0, 0.8, 0.9)).is_none());
        assert!(rule.observe(&observation(1, 0.1, 0.1)).is_none());
        assert_eq!(rule.active(), Some(false));
    }

    #[test]
    fn numeric_rule_honors_stability_initial_and_update_configuration() {
        let mut rule = NumericRule::new(NumericRuleConfig {
            id: RuleId::new(),
            event: "critical".into(),
            enter_below: 0.2,
            leave_above: 0.3,
            minimum_confidence: 0.0,
            required_samples: 1,
            sample_window: 1,
            cooldown: Duration::ZERO,
            stable_for: Duration::from_millis(200),
            emit_initial: true,
            update_interval: Some(Duration::from_millis(300)),
        })
        .unwrap();
        assert!(rule.observe(&observation(0, 0.1, 1.0)).is_none());
        assert!(rule.observe(&observation(199, 0.1, 1.0)).is_none());
        assert_eq!(
            rule.observe(&observation(200, 0.1, 1.0)).unwrap().state,
            TransitionState::Entered
        );
        assert!(rule.observe(&observation(499, 0.1, 1.0)).is_none());
        assert_eq!(
            rule.observe(&observation(500, 0.1, 1.0)).unwrap().state,
            TransitionState::Updated
        );
    }

    #[test]
    fn normalized_crop_rounds_outward() {
        assert_eq!(
            normalized_to_pixels(
                NormalizedRegion {
                    x: 0.1,
                    y: 0.2,
                    width: 0.25,
                    height: 0.5
                },
                100,
                50
            )
            .unwrap(),
            PixelRegion {
                x: 10,
                y: 10,
                width: 25,
                height: 25
            }
        );
    }

    fn health_frame(sequence: u64, fill: usize) -> Arc<Frame> {
        let mut bytes = vec![0_u8; 10 * 2 * 4];
        for y in 0..2 {
            for x in 0..10 {
                let offset = (y * 10 + x) * 4;
                bytes[offset..offset + 4].copy_from_slice(if x < fill {
                    &[220, 20, 20, 255]
                } else {
                    &[10, 10, 10, 255]
                });
            }
        }
        Arc::new(
            Frame::new(
                sequence,
                Duration::from_millis(sequence * 100),
                FrameLayout {
                    width: 10,
                    height: 2,
                    row_stride: 40,
                    format: PixelFormat::Rgba8,
                },
                Some("replay".into()),
                Arc::from(bytes),
            )
            .unwrap(),
        )
    }

    #[test]
    fn replay_frames_share_detector_and_rule_path_deterministically() {
        let detector_id = DetectorId::new();
        let element_id = ElementId::new();
        let build = || {
            FrameProcessor::new(
                AnalysisScheduler::new(10).unwrap(),
                ColorBarDetector::new(ColorBarConfig {
                    direction: yash_app_events_profile::BarDirection::LeftToRight,
                    minimum_rgb: [180, 0, 0],
                    maximum_rgb: [255, 60, 60],
                    line_match_fraction: 0.8,
                    maximum_gap_fraction: 0.02,
                    mask: None,
                })
                .unwrap(),
                NormalizedRegion {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
                detector_id,
                element_id,
                NumericRule::new(NumericRuleConfig {
                    id: RuleId::new(),
                    event: "critical_health".into(),
                    enter_below: 0.2,
                    leave_above: 0.3,
                    minimum_confidence: 0.0,
                    required_samples: 2,
                    sample_window: 3,
                    cooldown: Duration::ZERO,
                    stable_for: Duration::ZERO,
                    emit_initial: false,
                    update_interval: None,
                })
                .unwrap(),
            )
        };
        let frames: Vec<_> = [8, 8, 1, 1, 1, 4, 4]
            .into_iter()
            .enumerate()
            .map(|(index, fill)| health_frame(index as u64, fill))
            .collect();
        let run = |mut processor: FrameProcessor<ColorBarDetector>| {
            ReplaySource::new(frames.clone())
                .filter_map(|frame| processor.process(&frame)?.transition)
                .map(|transition| transition.state)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            run(build()),
            vec![TransitionState::Entered, TransitionState::Left]
        );
        assert_eq!(
            run(build()),
            vec![TransitionState::Entered, TransitionState::Left]
        );
    }

    #[test]
    fn replay_metrics_detect_regressions_and_latency() {
        let rule_id = RuleId::new();
        let observed = vec![
            Transition {
                rule_id,
                event: "critical".into(),
                timestamp_ms: 120,
                state: TransitionState::Entered,
                value: 0.1,
                confidence: 1.0,
            },
            Transition {
                rule_id,
                event: "critical".into(),
                timestamp_ms: 125,
                state: TransitionState::Entered,
                value: 0.1,
                confidence: 1.0,
            },
        ];
        let metrics = evaluate_replay(
            &[ExpectedEvent {
                event: "critical".into(),
                state: TransitionState::Entered,
                timestamp_ms: 100,
                tolerance_ms: 50,
            }],
            &observed,
            &ReplayRegression::default(),
        );
        assert_eq!(
            (metrics.matched, metrics.duplicates, metrics.misses),
            (1, 1, 0)
        );
        assert_eq!(metrics.mean_latency_ms, Some(20.0));
        assert!(!metrics.passed);
    }
}
