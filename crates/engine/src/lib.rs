//! Latest-frame scheduling, typed observations, temporal rules, and transitions.

use std::collections::VecDeque;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use yash_app_events_profile::{DetectorId, ElementId, NormalizedRegion, RuleId};

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
    state: Option<bool>,
    evidence: VecDeque<bool>,
    last_transition: Option<Duration>,
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
        } = config;
        if !enter_below.is_finite()
            || !leave_above.is_finite()
            || leave_above < enter_below
            || !(0.0..=1.0).contains(&minimum_confidence)
            || required_samples == 0
            || required_samples > sample_window
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
            state: None,
            evidence: VecDeque::with_capacity(sample_window),
            last_transition: None,
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
        let previous = self.state;
        if next == previous {
            return None;
        }
        self.state = next;
        previous?;
        let timestamp = observation.monotonic_timestamp();
        if self
            .last_transition
            .is_some_and(|last| timestamp.saturating_sub(last) < self.cooldown)
        {
            self.state = previous;
            return None;
        }
        self.last_transition = Some(timestamp);
        Some(Transition {
            rule_id: self.id,
            event: self.event.clone(),
            timestamp_ms: observation.timestamp_ms,
            state: if next == Some(true) {
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
    Left,
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

#[cfg(test)]
mod tests {
    use super::*;
    use yash_app_events_profile::{DetectorId, ElementId, RuleId};

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
        })
        .unwrap();
        assert!(rule.observe(&observation(0, 0.8, 0.9)).is_none());
        assert!(rule.observe(&observation(1, 0.1, 0.1)).is_none());
        assert_eq!(rule.active(), Some(false));
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
}
