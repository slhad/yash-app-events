# First-release security and privacy review

Reviewed 2026-07-11 against protocol/profile/capture/output schema version 1.

- Control is local-only: the daemon binds no TCP listener, creates its runtime
  directory as `0700`, its Unix socket as `0600`, rejects unsafe stale paths, limits
  connections, messages to 1 MiB, nesting to 32, and subscription buffers to 64.
- Portable imports are untrusted: staged ZIP parsing rejects traversal, absolute
  paths, link entries, undeclared resources, size/count/expansion excess, checksum
  mismatch, unsupported schemas, invalid profiles, and missing assets. Imported data
  is never executed.
- Portable ONNX models are regular profile-relative files capped at 64 MiB and must
  match the profile-declared SHA-256 before ONNX Runtime creates a CPU session. Input
  dimensions, label count, preprocessing, scheduling, and output cardinality are
  bounded; failures become error observations.
- Portable exports exclude machine-local portal tokens, node identifiers, and capture
  bindings. Tokens remain in atomic local configuration below XDG config.
- Frame queues are bounded to the latest frame. Preview images are opt-in, bounded,
  compressed, per-connection leased, and discarded on disconnect. Detector diagnostic
  previews are bounded and returned in memory only.
- Image persistence requires the explicit snapshot or template-capture action. Paths
  are written atomically. The GUI labels these actions and the capture state/source is
  visible through GUI, CLI, status RPC, and state output.
- Logs contain operational categories and identifiers, not raw frame bytes or restore
  tokens. `RUST_LOG` controls verbosity.
- Image replay accepts only profile-relative, non-traversing PNG paths, caps files at
  16 MiB, dimensions at 4096×4096, sample count at 10,000, and supported pixels to
  8-bit grayscale/RGB/RGBA.
- Diagnostic bundles require plan/review/export. The plan discloses every redacted
  entry and size. Only explicitly selected frozen element regions become crops; full
  frames are never implicit. Recursive redaction excludes tokens, secrets, credentials,
  capture bindings, portal sessions, and window IDs. Count/file/total limits and atomic
  failure tests protect export.
- Event and state output is daemon-owned. State/config writes use same-directory
  temporary files, flush/sync/rename, and failure tests retain the previous valid file.

Known release limitations: there is no remote control transport or authentication
because no network listener exists. A user with access to the same Unix account can
read that account's files and connect to its socket. Portal/compositor capture
indicators and permission revocation are desktop responsibilities.
