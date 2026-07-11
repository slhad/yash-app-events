//! Backend-neutral capture frame and source interfaces.

/// Pixel encodings accepted at the capture boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelFormat {
    /// Eight-bit red, green, blue, and alpha channels.
    Rgba8,
}
