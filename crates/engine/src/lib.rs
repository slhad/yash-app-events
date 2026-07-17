//! Latest-frame scheduling, typed observations, temporal rules, and transitions.

pub mod collection;
pub mod suite;

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use yash_app_events_capture::Frame;
use yash_app_events_profile::{
    AtomicRulePredicate, DetectorId, ElementId, NormalizedRegion, ObservationCondition, RuleId,
    RulePredicate,
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
        let evaluations = self
            .conditions
            .iter()
            .map(|condition| {
                let observation = self.latest.get(&condition.element_id)?;
                atomic_matches(&condition.predicate, &observation.value)
                    .map(|matched| (matched, observation.confidence.unwrap_or(1.0)))
            })
            .collect::<Option<Vec<_>>>()?;
        let matched = if self.require_all {
            evaluations.iter().all(|(matched, _)| *matched)
        } else {
            evaluations.iter().any(|(matched, _)| *matched)
        };
        let confidence = evaluations
            .iter()
            .map(|(_, confidence)| *confidence)
            .reduce(f32::min)
            .unwrap_or(1.0);
        let synthetic = Observation {
            detector_id: observation.detector_id,
            element_id: observation.element_id,
            timestamp_ms: observation.timestamp_ms,
            value: ObservationValue::Boolean(matched),
            confidence: Some(confidence),
            status: ObservationStatus::Valid,
            diagnostic: "composed observation evidence".into(),
        };
        self.temporal.observe(&synthetic)
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
        let now = observation.monotonic_timestamp();
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
            return Some(self.transition(observation, confidence, desired));
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
                observation,
                confidence,
                TransitionState::Updated,
            ));
        }
        None
    }

    fn transition(&self, observation: &Observation, confidence: f32, active: bool) -> Transition {
        let state = if active {
            TransitionState::Entered
        } else {
            TransitionState::Left
        };
        self.transition_with_state(observation, confidence, state)
    }

    fn transition_with_state(
        &self,
        observation: &Observation,
        confidence: f32,
        state: TransitionState,
    ) -> Transition {
        Transition {
            rule_id: self.config.id,
            event: self.config.event.clone(),
            timestamp_ms: observation.timestamp_ms,
            state,
            value: numeric_transition_value(&observation.value),
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
    use std::sync::Arc;

    use super::*;
    use yash_app_events_capture::{Frame, FrameLayout, PixelFormat, ReplaySource};
    use yash_app_events_profile::{DetectorId, ElementId, RuleId};
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
