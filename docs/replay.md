# Replay manifests and evaluation

Replay manifest schema 1 is a portable JSON document for deterministic, legally
redistributable synthetic detector inputs. `profile_id` and `element_id` select an
installed profile element; `values` are detector-specific synthetic samples spaced
100 ms apart. The daemon builds frames from those samples and sends them through the
same configured detector, scheduler, and event rule used by live capture.

For OCR, seven-segment, and classifier profiles, replace `values` with `image_frames`, a list of
profile-relative PNG assets. Exactly one input list must contain 1–10,000 entries.
Image paths cannot be absolute or traverse parents; files are capped at 16 MiB and
decoded images must be 8-bit grayscale/RGB/RGBA at no more than 4096×4096.

```json
{
  "schema": 1,
  "profile_id": "00000000-0000-0000-0000-000000000001",
  "element_id": "00000000-0000-0000-0000-000000000002",
  "values": [8, 8, 1, 1, 1, 4, 4],
  "expected_events": [
    {"event":"critical_health","state":"entered","timestamp_ms":300,"tolerance_ms":100},
    {"event":"critical_health","state":"left","timestamp_ms":600,"tolerance_ms":100}
  ],
  "regression": {
    "minimum_precision": 1.0,
    "minimum_recall": 1.0,
    "maximum_mean_latency_ms": 100.0
  }
}
```

Image example:

```json
{
  "schema": 1,
  "profile_id": "00000000-0000-0000-0000-000000000001",
  "element_id": "00000000-0000-0000-0000-000000000002",
  "image_frames": ["replay/menu.png", "replay/victory.png"],
  "expected_events": [
    {"event":"victory","state":"entered","timestamp_ms":100,"tolerance_ms":0}
  ]
}
```

Run `yash-eventsctl --json replay manifest.json`. The JSON result contains observed
events and event-level precision, recall, duplicates, misses, and mean absolute
latency. Exit status 7 means the configured thresholds were not met. Invalid schema,
unknown profile/element IDs, missing rules, and fixtures outside 1–10,000 samples are
rejected before evaluation.

Synthetic values mean:

- color bar: filled tenths from 0 through 10;
- template: zero produces an inverted template and any nonzero value the template;
- region change: grayscale intensity from 0 through 255.
- OCR/seven-segment/classifier: bounded profile-relative PNG frames through `image_frames`.

Replay evaluation never writes captures. It does publish observations and transitions
through the same durable state/event, subscription, and enabled profile output-route
boundary used by live processing. This makes a reviewed file or direct-command route
testable with recorded frames; machine-local route enablement remains explicit.

## External real-game suites

Keep copyrighted or private captures outside version control. The root `/assets/`
directory is ignored for convenient local packages, but `suite.evaluate` accepts a
package located in any readable directory. A package contains `suite.json`, a pinned
portable `profile.json`, case JSON documents, and the referenced PNG media. The suite's
`files` array inventories every referenced file with SHA-256; missing, changed,
absolute, parent-traversing, or package-escaping paths are rejected before detection.

Each frame declares one placement:

- `{"type":"full_frame"}` evaluates an original reference-sized screenshot;
- `{"type":"partial_frame","source_region":{"x":0,"y":0,"width":530,"height":254}}`
  places an existing partial screenshot at those reference-resolution pixels;
- `{"type":"zone_crop","target":"stage_group"}` scales an exact crop into the
  named detector zone, which makes small screenshots useful for calibration.

Observation targets may use stable element IDs or normalized element/derived names.
Expected values are typed JSON booleans, numbers, or strings. `numeric_tolerance` is
opt-in; exact comparison remains the default. `minimum_confidence` and the expected
status may also be asserted. Cases may additionally set `check_events: true` and use
the same `expected_events` contract as replay manifests. `source_media` records which
original screenshot or footage produced extracted frames without requiring video
decoding during every regression run.

Minimal case:

```json
{
  "schema": 1,
  "id": "partial-stage-5-06",
  "purpose": "Distinguish Stage 5 and retain the zero-padded counter",
  "categories": ["zone_crop", "stage", "timer"],
  "source_media": "media/partial/stage-5-purple.png",
  "frames": [{
    "image": "media/crops/stage-5-group.png",
    "placement": {"type": "zone_crop", "target": "stage_group"},
    "expected_observations": {
      "stage_group": {"status": "valid", "value": "5"}
    }
  }]
}
```

Run `yash-eventsctl --json suite evaluate /path/to/package`. The client resolves the
path before sending it to the daemon and allows 60 seconds by default. Results include
per-case and per-category totals, every typed assertion with its actual observation,
optional event metrics, and an overall `passed` flag. Exit status 7 means a regression;
invalid packages remain JSON-RPC errors.

## Passive evidence inbox

An opt-in machine-local collection policy can point an active profile at an external
package. While that game's capture is active, the daemon samples the exact analyzed
frame every 70 seconds by default (with configurable jitter), never from a stale or
unbounded queue. It writes `inbox/<item-id>/frame.png` and `metadata.json` atomically.
Metadata includes the pinned profile revision/hash, image hash, capture geometry and
sequence, all aliased observations, emitted transitions, novelty reason, a compact
32×18 grayscale thumbnail, and review state.

Similarity alone is not enough to discard evidence: a frame is skipped only when its
normalized thumbnail difference is at or below the configured threshold and its
detector-evidence signature is unchanged with no event transition. Values are included
in that signature only for configured novelty targets. Item and byte quotas stop new
writes visibly instead of deleting old evidence.

Use `collection items` and `collection get` (or the GUI's Passive evidence collector)
to inspect a batch. Items begin as `pending`; review actions are `accept`, `correct`,
`reject`, and `promote`. Correct/promotion expectations use the same typed assertion
objects as suite cases. Safe automatic review verifies hashes, rejects equivalent
duplicates, promotes only sufficiently confident novel values when requested, and
marks uncertain OCR as `needs_correction` rather than treating predictions as truth.
Promotion copies the frame into `media/collected`, creates a case under `cases/`, and
atomically updates `suite.json` plus its SHA-256 inventory. Run `suite evaluate` after
promotion to verify the enlarged package through the normal detector path.
