//! Backend-neutral capture frames, sources, and bounded latest-frame delivery.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use thiserror::Error;

/// Pixel encodings accepted at the capture boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelFormat {
    /// Eight-bit red, green, blue, and alpha channels.
    Rgba8,
    /// Eight-bit red, green, and blue channels.
    Rgb8,
}

impl PixelFormat {
    #[must_use]
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgba8 => 4,
            Self::Rgb8 => 3,
        }
    }
}

/// CPU-backed frame shared without copying between capture and analysis.
#[derive(Clone, Debug)]
pub struct Frame {
    pub sequence: u64,
    pub timestamp: Duration,
    pub width: u32,
    pub height: u32,
    pub row_stride: usize,
    pub format: PixelFormat,
    pub source_id: Option<String>,
    pub data: Arc<[u8]>,
}

/// Dimensions and memory layout negotiated by a capture backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameLayout {
    pub width: u32,
    pub height: u32,
    pub row_stride: usize,
    pub format: PixelFormat,
}

impl Frame {
    /// Constructs a validated packed or padded frame.
    ///
    /// # Errors
    ///
    /// Rejects zero dimensions, short strides, overflow, and mismatched buffers.
    pub fn new(
        sequence: u64,
        timestamp: Duration,
        layout: FrameLayout,
        source_id: Option<String>,
        data: Arc<[u8]>,
    ) -> Result<Self, FrameError> {
        let FrameLayout {
            width,
            height,
            row_stride,
            format,
        } = layout;
        if width == 0 || height == 0 {
            return Err(FrameError::ZeroDimensions);
        }
        let packed = usize::try_from(width)
            .map_err(|_| FrameError::Overflow)?
            .checked_mul(format.bytes_per_pixel())
            .ok_or(FrameError::Overflow)?;
        if row_stride < packed {
            return Err(FrameError::ShortStride);
        }
        let required = row_stride
            .checked_mul(usize::try_from(height).map_err(|_| FrameError::Overflow)?)
            .ok_or(FrameError::Overflow)?;
        if data.len() != required {
            return Err(FrameError::BufferSize {
                expected: required,
                actual: data.len(),
            });
        }
        Ok(Self {
            sequence,
            timestamp,
            width,
            height,
            row_stride,
            format,
            source_id,
            data,
        })
    }
}

/// Frame construction failure.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum FrameError {
    #[error("frame dimensions must be non-zero")]
    ZeroDimensions,
    #[error("frame size arithmetic overflow")]
    Overflow,
    #[error("row stride is shorter than packed pixels")]
    ShortStride,
    #[error("frame buffer size mismatch: expected {expected}, actual {actual}")]
    BufferSize { expected: usize, actual: usize },
}

#[derive(Debug, Default)]
struct SlotState {
    frame: Option<Arc<Frame>>,
    replacements: u64,
}

/// A constant-memory handoff where a new capture frame replaces any unconsumed frame.
#[derive(Clone, Debug, Default)]
pub struct LatestFrameSlot(Arc<Mutex<SlotState>>);

impl LatestFrameSlot {
    /// Publishes immediately and returns whether an unconsumed frame was replaced.
    pub fn publish(&self, frame: Arc<Frame>) -> bool {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let replaced = state.frame.replace(frame).is_some();
        if replaced {
            state.replacements = state.replacements.saturating_add(1);
        }
        replaced
    }

    /// Takes the newest available frame, leaving the slot empty.
    pub fn take(&self) -> Option<Arc<Frame>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .frame
            .take()
    }

    /// Number of unconsumed frames replaced since creation.
    #[must_use]
    pub fn replacements(&self) -> u64 {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replacements
    }
}

/// Deterministic timestamped replay source using the live frame boundary.
#[derive(Clone, Debug)]
pub struct ReplaySource {
    frames: Vec<Arc<Frame>>,
    next: usize,
}

impl ReplaySource {
    #[must_use]
    pub fn new(frames: Vec<Arc<Frame>>) -> Self {
        Self { frames, next: 0 }
    }
}

impl Iterator for ReplaySource {
    type Item = Arc<Frame>;
    fn next(&mut self) -> Option<Self::Item> {
        let frame = self.frames.get(self.next).cloned();
        self.next = self.next.saturating_add(usize::from(frame.is_some()));
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(sequence: u64) -> Arc<Frame> {
        Arc::new(
            Frame::new(
                sequence,
                Duration::from_millis(sequence * 10),
                FrameLayout {
                    width: 1,
                    height: 1,
                    row_stride: 4,
                    format: PixelFormat::Rgba8,
                },
                Some("synthetic".into()),
                Arc::from([0, 0, 0, 255]),
            )
            .unwrap(),
        )
    }

    #[test]
    fn latest_slot_never_accumulates_stale_frames() {
        let slot = LatestFrameSlot::default();
        for sequence in 0..10_000 {
            slot.publish(frame(sequence));
        }
        assert_eq!(slot.replacements(), 9_999);
        assert_eq!(slot.take().unwrap().sequence, 9_999);
        assert!(slot.take().is_none());
    }

    #[test]
    fn frame_rejects_short_buffer_and_stride() {
        assert!(matches!(
            Frame::new(
                0,
                Duration::ZERO,
                FrameLayout {
                    width: 2,
                    height: 1,
                    row_stride: 3,
                    format: PixelFormat::Rgba8
                },
                None,
                Arc::from([0; 3])
            ),
            Err(FrameError::ShortStride)
        ));
    }
}
