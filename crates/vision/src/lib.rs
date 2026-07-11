//! Deterministic detector implementations and preprocessing.

/// Detector failures never imply a negative observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DetectionStatus {
    /// The detector produced a meaningful value.
    Valid,
    /// The detector lacked sufficient evidence.
    Unknown,
    /// The detector could not evaluate the input.
    Error,
}
