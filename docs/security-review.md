# First-release security and privacy review

Reviewed 2026-07-11 against protocol/profile/capture/output schema version 1.

- Control is local-only: the daemon binds no TCP listener, creates its runtime
  directory as `0700`, its Unix socket as `0600`, rejects unsafe stale paths, limits
  connections, messages to 1 MiB, nesting to 32, and subscription buffers to 64.
- Portable imports are untrusted: staged ZIP parsing rejects traversal, absolute
  paths, link entries, undeclared resources, size/count/expansion excess, checksum
  mismatch, unsupported schemas, invalid profiles, and missing assets. Imported data
  is never executed.
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
- Event and state output is daemon-owned. State/config writes use same-directory
  temporary files, flush/sync/rename, and failure tests retain the previous valid file.

Known release limitations: there is no remote control transport or authentication
because no network listener exists. A user with access to the same Unix account can
read that account's files and connect to its socket. Portal/compositor capture
indicators and permission revocation are desktop responsibilities.
