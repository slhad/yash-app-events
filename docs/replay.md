# Replay manifests and evaluation

Replay manifest schema 1 is a portable JSON document for deterministic, legally
redistributable synthetic detector inputs. `profile_id` and `element_id` select an
installed profile element; `values` are detector-specific synthetic samples spaced
100 ms apart. The daemon builds frames from those samples and sends them through the
same configured detector, scheduler, and event rule used by live capture.

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

Run `yash-eventsctl --json replay manifest.json`. The JSON result contains observed
events and event-level precision, recall, duplicates, misses, and mean absolute
latency. Exit status 7 means the configured thresholds were not met. Invalid schema,
unknown profile/element IDs, missing rules, and fixtures outside 1–10,000 samples are
rejected before evaluation.

Synthetic values mean:

- color bar: filled tenths from 0 through 10;
- template: zero produces an inverted template and any nonzero value the template;
- region change: grayscale intensity from 0 through 255.

Replay evaluation never writes captures. The older replay RPC that demonstrates
durable output remains separate because evaluation must not contaminate live output.
