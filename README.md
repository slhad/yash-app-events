# yash-app-events

`yash-app-events` is a planned Linux-first HUD observation and event service for games that do not expose native telemetry APIs.

It will capture a selected game window through the Wayland ScreenCast portal and PipeWire, analyze user-configured regions, turn visual observations into debounced state transitions, and expose results through JSON files, a CLI, and local JSON-RPC IPC.

> Status: engineering baseline implemented. The workspace builds and its placeholder
> binaries run. The internal profile library now provides validated schema-v1 storage,
> revision recovery, duplication, trash/restore, and safe portable archives. The local
> protocol-v1 daemon and CLI profile/status operations work; capture, detection, event
> output, and the GUI workflow are not implemented yet.

## Intended use cases

- Detect critical health or resource levels.
- Detect victory, defeat, run start, and run end screens.
- Recognize a new level or area name.
- Detect known pets, items, icons, or game modes.
- Feed game events into overlays, automations, stream tooling, or accessibility helpers.

This is a visual observation tool. It does not read game memory, inject into the game, or generate game input.

## Planned components

```text
Wayland portal + PipeWire
          |
          v
  yash-app-eventsd
    |     |      |
    |     |      +--> state.json / events.jsonl
    |     +---------> JSON-RPC over Unix socket
    +---------------> detector and event engine
                         ^              ^
                         |              |
                     GUI editor      yash-eventsctl
```

- `yash-app-eventsd`: capture, detection, state, persistence, output, and IPC daemon.
- `yash-app-events`: egui-based visual profile and region editor.
- `yash-eventsctl`: scriptable command-line client.

Names are provisional until the first CLI milestone; once published, command names become compatibility-sensitive.

## Planned Linux requirements

- A Wayland desktop with a working `xdg-desktop-portal` ScreenCast backend.
- PipeWire.
- A Rust toolchain for source builds.
- Native development dependencies required by the selected PipeWire, GUI, and OpenCV bindings.

Exact distribution-specific packages and build commands will be added after the workspace compiles in CI and on at least one supported Linux distribution.

## Planned workflow

1. Start the daemon or let socket activation start it.
2. Open the GUI.
3. Select a game window through the desktop portal.
4. Freeze or inspect the live preview.
5. Draw normalized HUD regions.
6. Assign a detector to each region.
7. Convert observations into temporal event rules.
8. Test the profile against live frames or a replay.
9. Save or export the profile.
10. Consume events from files, the CLI, or JSON-RPC subscriptions.

Implemented CLI usage:

```bash
yash-eventsctl status
yash-eventsctl profile list
yash-eventsctl profile create "My game" my_game
yash-eventsctl profile validate ./profile.json
yash-eventsctl profile activate <profile-uuid>
yash-eventsctl events follow --json
yash-eventsctl state --json
```

The daemon and live commands require `XDG_RUNTIME_DIR`; offline profile validation does
not require a running daemon. All commands accept `--json`, `--socket`, and
`--timeout-ms`.

## Configuration and output

Portable profiles will live below the XDG data directory and contain a versioned profile document plus relative template/model assets. Machine-local capture bindings will live separately below the XDG config directory.

Runtime output will provide:

- `events.jsonl`: append-only meaningful state transitions.
- `state.json`: atomically replaced current state snapshot.
- JSON-RPC subscriptions: live events for connected clients.

See `SPECS.md` for normative schemas and behavior.

## Development

The Rust workspace and crate boundaries are initialized. Implementation order and
acceptance gates are in `PLAN.md`; dependency direction is documented in
`docs/architecture.md`.

The canonical baseline checks are:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
bash scripts/check-readme-claims.sh
cargo doc --workspace --no-deps
```

## Project principles

- Linux/Wayland first, with replaceable capture backends.
- Raw continuous capture, but bounded 5–10 FPS analysis by default.
- Latest frame wins; stale frames are dropped.
- Deterministic vision before OCR or neural inference.
- Observations are not events; temporal rules produce events.
- Portable, versioned, recoverable configuration.
- One daemon and one protocol for GUI, CLI, and integrations.
- Inspectable outputs and replay-based verification.
