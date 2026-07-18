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
- numeric, boolean, text, stable-duration, composed, initial, and updated event rules;
- deterministic color/template/change/seven-segment, OCR, and portable ONNX classifier configuration and diagnostics;
- diagnostic bundle entry/size/privacy review and confirmed export.
- passive evidence policy/status, an inspectable image/observation batch, editable typed
  expectations, accept/correct/reject/promote actions, and conservative automatic
  review through the shared collection JSON-RPC methods.
- machine-local output route listing, enable/disable controls, trigger/sink inspection,
  and explicit sample delivery through the shared output JSON-RPC methods.
- packaged inert output-recipe browsing with provenance/hash disclosure, editable
  trigger/payload JSON, side-effect-free preview, explicit local sink selection, and
  disabled installation before the existing test/enable controls.
- collapsed public Profile Catalog browsing with cached/offline status, compatibility,
  media/provenance/license/verification disclosure, explicit review, and inactive install.

Preview is a per-connection lease. The daemon downsamples to at most 320×180 and
returns a compressed PNG from a clone of the latest frame; detector input is unchanged.
Disconnecting the GUI drops the lease automatically, and preview never writes a file.

Automated evidence includes normalized interaction tests, preview lease/downscale PNG
tests, strict workspace Clippy/tests, and native Wayland picker/preview/configuration
acceptance alongside the daemon on 2026-07-11.
