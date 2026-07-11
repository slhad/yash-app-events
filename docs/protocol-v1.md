# Control protocol version 1

The daemon listens only on `$XDG_RUNTIME_DIR/yash-app-events/control.sock`. The
runtime directory is mode `0700` and the socket is mode `0600`. Each message is one
compact JSON object followed by `\n`; messages are limited to 1 MiB and nesting depth
32. Each connection must first call `system.handshake` with protocol `1`, client name,
and client version.

Requests and responses use JSON-RPC 2.0. Version 1 defines:

- `system.handshake`, `system.version`, `system.capabilities`, `system.status`,
  `system.shutdown`
- `profile.list`, `profile.get`, `profile.create`, `profile.commit`,
  `profile.duplicate`, `profile.validate`, `profile.import`, `profile.export`,
  `profile.trash`, `profile.restore`, `profile.activate`
- `profile.draft` (recoverable, separate from the committed revision)
- `state.get`, `events.subscribe`, `status.subscribe`
- `replay.synthetic_health` (CI-safe synthetic vertical-slice fixture)
- `replay.evaluate` (schema-v1 annotated manifest, observed transitions, metrics and
  regression result)
- `detector.test` (bounded synthetic input plus compressed PNG diagnostic preview;
  never persists its test frame)
- `detector.capture_template` (explicit latest-frame normalized crop into a portable
  profile asset)
- `replay.profile_detector` (CI-safe profile detector/rule replay through durable and
  live outputs)
- `capture.select`, `capture.start`, `capture.stop`, `capture.status`, and
  `capture.snapshot` (daemon-owned local portal lifecycle; snapshots are explicit)
- `preview.start`, `preview.frame`, `preview.freeze`, `preview.unfreeze`, and
  `preview.stop` (per-connection lease, bounded compressed PNG, and exact frozen-frame
  detector testing)

Profile IDs are UUID strings. `profile.commit` accepts `profile` and
`expected_revision`; error `-32009` includes both expected and current revisions.
Import/export paths are local filesystem paths supplied by the current-user client.

Subscriptions have a 64-notification buffer per client. A lagging reader receives
`subscription.lagged` with error code `-32010` and the number skipped; it never blocks
the producer. Element create/update/delete/duplicate operations use revision-aware
`profile.commit`, keeping the daemon as the only profile writer.

Stable application errors are `-32001` handshake required, `-32002` incompatible
version, `-32009` revision conflict, and `-32010` subscriber lag. Standard JSON-RPC
parse/request/method/parameter/internal codes retain their standard meanings. The
method set above and its persisted schema-1/protocol-1 identifiers are frozen for the
first release; additive response fields remain compatible.
