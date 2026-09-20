# Control protocol version 1

The daemon listens only on `$XDG_RUNTIME_DIR/yash-app-events/control.sock`. The
runtime directory is mode `0700` and the socket is mode `0600`. Each message is one
compact JSON object followed by `\n`; messages are limited to 1 MiB and nesting depth
32. Each connection must first call `system.handshake` with protocol `1`, client name,
and client version.

Requests and responses use JSON-RPC 2.0. Version 1 defines:

- `system.handshake`, `system.version`, `system.capabilities`, `system.status`,
  `system.shutdown`
- `profile.list`, `profile.get`, `profile.revisions`, `profile.revision_get`,
  `profile.rollback`, `profile.create`, `profile.commit`,
  `profile.duplicate`, `profile.validate`, `profile.import`, `profile.export`,
  `profile.trash`, `profile.restore`, `profile.activate`
- `profile.draft` (recoverable, separate from the committed revision)
- `catalog.status`, `catalog.refresh`, `catalog.list`, and `catalog.install` (fixed-origin,
  bounded and cached public profile discovery; install requires the reviewed revision/hash
  and leaves the imported profile inactive)
- `state.get`, `events.subscribe`, `status.subscribe`
- `analysis.evaluate_image` (pure bounded PNG evaluation with scene/overlay context,
  spatial element/target JSON, no image bytes, and no state/event/output side effects)
- `output.list`, `output.set`, `output.enable`, `output.remove`, and `output.test`
  (machine-local profile routes; `test` performs an explicit sample delivery)
- `output.recipe_list`, `output.recipe_preview`, and `output.recipe_install` (portable
  inert examples; preview never executes and installation always creates a disabled route)
- `replay.synthetic_health` (CI-safe synthetic vertical-slice fixture)
- `replay.evaluate` (schema-v1 annotated manifest, observed transitions, metrics and
  regression result)
- `suite.evaluate` (schema-v1 external real-game package, pinned profile, verified
  media inventory, typed observations, optional events, and categorized result)
- `collection.policy_get`, `collection.policy_set`, `collection.status`,
  `collection.items`, `collection.item_get`, `collection.review`,
  `collection.auto_review`, and `collection.compare` (machine-local passive evidence,
  bounded review queue, conservative batch decisions, and suite promotion)
- `detector.test` (bounded synthetic input plus compressed PNG diagnostic preview;
  never persists its test frame)
- `detector.capture_template` (explicit latest-frame normalized crop into a portable
  profile asset)
- `replay.profile_detector` (CI-safe profile detector/rule replay through durable and
  live outputs)
- `capture.select`, `capture.start`, `capture.stop`, `capture.status`, and
  `capture.snapshot` (daemon-owned local portal lifecycle; snapshots are explicit)
- `capture.auto_get` and `capture.auto_set` (machine-local process-presence policy;
  automatic starts reuse the retained portal token and automatic stops release capture)
- `preview.start`, `preview.frame`, `preview.freeze`, `preview.unfreeze`, and
  `preview.stop` (per-connection lease, bounded compressed PNG, and exact frozen-frame
  detector testing)
- `diagnostic.plan` (exact redacted entries, sizes, selected-image flags, and privacy
  warning) and `diagnostic.export` (reviewed total confirmation plus atomic ZIP output)

Profile IDs are UUID strings. `profile.commit` accepts `profile` and
`expected_revision`; error `-32009` includes both expected and current revisions.
Import/export paths are local filesystem paths supplied by the current-user client.
The daemon-owned `profile.import` and catalog installation paths keep a fresh
archive's revision for a new profile ID. If the ID already has local lineage, they
rebase the imported document to the next known revision before publishing it. The
returned profile contains that final revision; portable archive contents are not
changed.

`analysis.evaluate_image` accepts exactly one of `profile_id` or `bundle`, an explicit local PNG
`path`, and a bounded `timeout_ms`. `profile_id` keeps the legacy single-profile behavior.
`bundle` names a bounded local profile-routing JSON document; the daemon derives all profile
directories from its pinned UUIDs and never executes paths from that document. The bundle pins
the router/member revisions and game slug, and maps member profiles to stable router scene and
overlay IDs. The daemon validates the router and selected member's revision, game, layout, and
route IDs before evaluating the selected member. Its response retains the normal selected-profile fields and adds
`profile_selection` with the router ID, route index, selected profile ID, matched context, and a
status of `selected`, `fallback`, `unresolved`, or `no_match`.

The request may also include one bounded `context` object. Its optional `previous` member carries
the prior profile ID/revision, recognized scene and overlay IDs, the prior frame SHA-256, and
`age_ms`; `expected_scene_ids`/`expected_overlay_ids` or
`expected_scene_names`/`expected_overlay_names` carry stable IDs or semantic names expected for
the next frame. Names are resolved against the selected profile. These values are only a soft
ranking prior. Every enabled scene and overlay is still evaluated, and the response's
`analysis_context` reports `status`, accepted preferred IDs and names, ignored reasons, and
`candidate_scope`/`fallback` set to `full_profile`. Profile/revision mismatches, stale context,
malformed hashes, over-limit values, and unknown IDs/names are ignored rather than treated as
recognition evidence. Context is never an image, coordinate, or action authorization.

Bundle analysis applies the context to the router; the router's `analysis_context` report is
preserved on the final response even when a specialized member is selected. Member analysis
remains independently bounded and validated.

The response includes the exact frame dimensions and SHA-256, ranked scene/overlay evidence,
stable names and IDs, normalized and pixel rectangles, optional profile-authored safe points,
detector scheduling metrics, and `ai_handoff`.
The handoff is JSON-only when the scene is unambiguous, layout-compatible, and every
explicitly required observation/visibility condition is valid. Otherwise it names at
most eight minimal crops; only a broad layout mismatch recommends a full frame. An OCR
detector may include a bounded `retry` policy with `after_ms`, `maximum_attempts`, and
an `expected_format`. When its required observation is missing or malformed,
`ai_handoff.retry.recommended` is true and `requests` names the affected element without
executing or scheduling the retry itself.

Bundle routing is bounded and deterministic: the highest `priority` matching member wins, then
the route with the most constrained scene/overlay dimensions, then manifest order. A fallback
member is used only when no constrained member matches. If no member matches and no fallback is
present, the router result is returned unchanged apart from `profile_selection`; the operation
does not activate a profile, mutate current state, publish events, execute output routes, or
persist the image.

Revision history is exposed without a GUI-only path. `profile.revisions` accepts
`profile_id` and returns retained profile snapshots from oldest to current.
`profile.revision_get` accepts `profile_id` and `revision`. `profile.rollback` accepts
`profile_id`, `revision`, and `expected_revision`; it commits the selected snapshot as
a new revision and returns that new current profile. It never deletes or rewrites the
revision that was current when rollback was requested.

Subscriptions have a 64-notification buffer per client. A lagging reader receives
`subscription.lagged` with error code `-32010` and the number skipped; it never blocks
the producer. Element create/update/delete/duplicate operations use revision-aware
`profile.commit`, keeping the daemon as the only profile writer. The portable rule
predicate `rapid_increase` accepts `minimum_delta` and `within_ms`; matching transitions
use the normal event subscription and durable output.

`system.status` includes process-wide daemon CPU percentage and resident-memory bytes
alongside capture/analysis rates and detector latency. CPU is derived from Linux
process user+system time over the sampling interval rather than one thread.

For schema-2 profiles, `state.get` adds `context` with the recognized scene, ranked
candidates, active/uncertain overlays, and evaluated/gated/throttled detector counts.
Subscriptions emit `scene_changed` and `overlays_changed` records when that context
changes; existing observation/event fields remain compatible.

`suite.evaluate` accepts `{"path":"/absolute/or/client-resolved/path","timings":false}`;
`timings` is optional and defaults to false. The path may
name a package directory or its `suite.json`. The daemon canonicalizes every referenced
path, verifies its SHA-256 inventory, loads the portable profile without installing it,
and returns case/frame/assertion/category totals plus per-assertion diagnostics and
optional replay event metrics. A suite result with `passed: false` is a successful RPC
response; the CLI maps it to exit status 7. When requested, an additive `timings` object
reports inventory, profile-load, total-case, and serialization milliseconds plus at
most ten slowest case IDs. It is omitted by default.

Long evaluations should use additive `suite.start`, `suite.status`, and `suite.cancel`.
`suite.start` accepts the evaluate parameters and returns an operation UUID immediately.
`suite.status` accepts `operation_id` plus optional `result`; it reports status, phase,
completed/total cases or files, current case, elapsed milliseconds, and a structured
terminal error. Requesting a completed result consumes the retained operation. Progress
is also emitted as bounded `suite_progress` notifications. `suite.cancel` is cooperative
and returns whether cancellation was accepted. At most four unconsumed operations are
retained.

`suite.start` admits at most one image workload and returns an error while another
foreground image job holds that slot. Synchronous image RPCs wait for the same slot
and run off the async control executor; status and cancellation remain available.
Native work retains its slot until it actually finishes, including after caller cancellation.

`state.get`, durable `state.json`, and state-triggered output routes receive one complete
snapshot after each analyzed frame with new observations. Individual transitions still
produce separate durable events, subscription notifications, and event-triggered output.

Collection policies are keyed by profile ID and remain machine-local. A policy contains
an absolute dataset root, enabled flag, interval/jitter, perceptual threshold, item/byte
quotas, and novelty-target names. Collection runs only on an active capture and stores
the same full-resolution frame that produced the attached observations. `review`
supports `accept`, `correct`, `reject`, and `promote`; correction/promotion accepts typed
expected-observation contracts. `auto_review` may reject verified duplicates and
accept/promote only trusted novel evidence, but sends ambiguous evidence to
`needs_correction`. `compare` applies the production 32×18 grayscale metric to two safe
PNG paths and reports their normalized difference and configured duplicate decision.

Output routes are also keyed by profile ID and remain machine-local. `output.list` accepts
`profile_id`; `output.set` accepts `profile_id` plus the complete schema-1 route;
`output.enable` accepts `profile_id`, `route_id`, and `enabled`; `output.remove` accepts
`profile_id` and `route_id`; `output.test` accepts the same identifiers and performs one
sample delivery, returning its rendered payload and receipt. See `docs/outputs.md` for the
route schema, security boundary, templates, and CLI examples.

`output.recipe_list` accepts `profile_id` and returns validated recipes with portable path
and SHA-256. Preview/install requests repeat `profile_id`, `recipe_id`, reviewed `sha256`,
and the edited `name`, `trigger`, and `format`. Preview returns the rendered sample JSON value or raw string with
`executed:false`. Install additionally requires a complete local sink, rejects a stale
hash, allocates a fresh route ID, records recipe provenance, and forces `enabled:false`.

Stable application errors are `-32001` handshake required, `-32002` incompatible
version, `-32009` revision conflict, and `-32010` subscriber lag. Standard JSON-RPC
parse/request/method/parameter/internal codes retain their standard meanings. The
method set above and its protocol-1 identifiers are frozen for the
first release; additive response fields remain compatible.
