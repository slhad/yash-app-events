# GUI foundation

`yash-app-events` is an eframe/egui protocol-v1 client. Its render thread only owns
widget state, texture upload, and normalized-coordinate interaction. A dedicated
thread owns the Tokio runtime, Unix RPC connection, reconnects, timeouts, and PNG
decoding. The daemon remains the only owner of capture, profiles, drafts, and outputs.

Implemented controls include:

- profile create, rename/save, duplicate, activate, import/export, trash, and restore;
- recoverable draft autosave, revert, validation/RPC errors, and revision-conflict
  visibility;
- portal source selection, stop, metrics, opt-in preview, and freeze;
- aspect-preserving preview, zoom, pan, draw/select/move/resize/duplicate regions,
  enable state, labels, normalized coordinates, and reference-pixel sizes.

Preview is a per-connection lease. The daemon downsamples to at most 320×180 and
returns a compressed PNG from a clone of the latest frame; detector input is unchanged.
Disconnecting the GUI drops the lease automatically, and preview never writes a file.

Automated evidence includes normalized interaction tests, preview lease/downscale PNG
tests, strict workspace Clippy/tests, and a five-second native Wayland launch alongside
the daemon on 2026-07-11. Live picker/preview interaction remains part of the manual
capture smoke gate.
