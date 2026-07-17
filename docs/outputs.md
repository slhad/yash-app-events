# Profile-scoped output routes

The daemon can route selected results for an active profile to additional machine-local
outputs. Routes are stored in `$XDG_CONFIG_HOME/yash-app-events/output-routes.json`; they
are deliberately excluded from portable `.hudprofile` archives. A profile may carry inert
recipes, but importing it can never install or authorize a file or command route.

Routes support:

- `event` triggers filtered by stable event name and transition state;
- `state_change` triggers, deduplicated by the rendered payload;
- `full` output using the stable event or state contract;
- `json_template` output;
- `text_template` output written as raw UTF-8 without JSON quoting;
- `file` sinks in `append` or atomic `replace` mode;
- `command` sinks using a direct absolute executable path, never a shell.

JSON command routes receive one compact JSON object plus a newline on standard input;
text routes receive raw UTF-8 with their configured trailing-newline policy. Standard
output and error are discarded. Arguments, template size, route count, pending deliveries,
and execution time are bounded. A failed route updates daemon `output_error` and publishes
an `output_route_error` notification without stopping capture.

## JSON template placeholders

The template source has `kind`, `event`, and `state`. Dotted placeholders address fields,
for example `{{event.event}}`, `{{event.state}}`, `{{event.value}}`,
`{{event.sequence}}`, or `{{state.observations}}`.

When a placeholder occupies the complete JSON string, its original JSON type is preserved:

```json
{"stage":"{{event.value}}"}
```

If `event.value` is the number `2`, the rendered value is `{"stage":2}`. Placeholders
embedded in a larger string are rendered as text.

## Raw text templates

`text_template` uses the same dotted placeholders, but always renders plain UTF-8. By
default the daemon adds one trailing line feed and never adds a carriage return, JSON
quotes, or an object envelope. Set `"trailing_newline": false` to write no line-ending
bytes at all. An
atomic `replace` file is therefore suitable as a universally readable current-value file:

```json
{
  "kind": "text_template",
  "template": "{{state.observations.0db480ba-90ac-43c1-9656-fe22c5903069.value.value}}",
  "trailing_newline": false
}
```

For the packaged BlazBlue profile this produces exactly the bytes `STAGE-3 : 04` with no
line ending. State-route deduplication compares the rendered text, so unrelated HUD
updates do not rewrite an unchanged stage.
Before a referenced derived observation exists, the background state router waits without
writing a file or reporting an output failure. The explicit **Test output** action still
reports a missing placeholder so a genuinely incorrect template remains visible.

## Portable route recipes

A profile author may place up to 32 schema-1 JSON recipes under `output-recipes/`. Each
file is limited to 64 KiB, validated during import/export, declared and hashed by the
portable archive manifest, and shown with its SHA-256 during review.

An inert command recipe looks like:

```json
{
  "schema": 1,
  "id": "d56f45d0-2eb2-45d7-8055-864438649071",
  "name": "Yash stage marker",
  "description": "Suggest one marker for each detected BlazBlue stage change.",
  "trigger": {
    "kind": "event",
    "events": ["stage_changed"],
    "states": ["updated"]
  },
  "format": {
    "kind": "json_template",
    "template": {
      "marker": "stage-{{event.value}}",
      "stage": "{{event.value}}"
    }
  },
  "suggested_sink": {
    "kind": "command",
    "program_name": "yash",
    "args": ["ipc", "command", "marker"],
    "timeout_ms": 5000
  }
}
```

Notice that `program_name` is only explanatory: the recipe has no executable path,
destination path, route ID, or enabled state. A file recipe similarly carries only a safe
suggested filename and append/replace intent.

The GUI installation flow is deliberately split:

1. Select a packaged example and inspect its description, source path, hash, trigger,
   disclosed output template, and suggested sink.
2. Edit the route name, trigger, or output template if needed.
3. **Preview output (no side effect)** using a synthetic event/current state.
4. Choose an absolute local executable or file destination and review arguments/timeout.
5. **Install disabled**. The daemon rechecks the reviewed recipe hash, allocates a fresh
   local route ID, and records profile/recipe/hash provenance.
6. Use the separate **Test output** action, then enable the installed route explicitly.

If the portable recipe changes after review, preview/install fails until it is reloaded.
Manually replacing an installed route clears its recipe provenance.

## File route example

Create `stage-marker.json`:

```json
{
  "id": "5efc08e7-e376-46e8-ae9d-aadbf3f159ae",
  "name": "BlazBlue stage markers",
  "enabled": false,
  "trigger": {
    "kind": "event",
    "events": ["stage_changed"],
    "states": ["entered", "updated"]
  },
  "format": {
    "kind": "json_template",
    "template": {
      "marker": "stage-{{event.value}}",
      "stage": "{{event.value}}",
      "sequence": "{{event.sequence}}"
    }
  },
  "sink": {
    "kind": "file",
    "path": "/absolute/path/to/stage-markers.jsonl",
    "mode": "append"
  }
}
```

Install, review, test, and enable it:

```bash
yash-eventsctl output set <profile-id> ./stage-marker.json
yash-eventsctl output list <profile-id> --json
yash-eventsctl output test <profile-id> 5efc08e7-e376-46e8-ae9d-aadbf3f159ae --json
yash-eventsctl output enable <profile-id> 5efc08e7-e376-46e8-ae9d-aadbf3f159ae --enabled true
```

The GUI **Output routes** panel presents the same routes, enable switches, sink details,
and explicit **Test output** action.

## Direct-command route

Replace the sink with a reviewed absolute executable and argument vector:

```json
{
  "kind": "command",
  "program": "/absolute/path/to/yash",
  "args": ["ipc", "command", "marker"],
  "timeout_ms": 5000
}
```

This is the intended extension point for a later BlazBlue stage-change marker. The exact
`yash` marker arguments should be verified against that application's IPC contract before
enabling the route; the event JSON remains available on standard input.
