# yash-app-events

`yash-app-events` is a Linux-first HUD observation and event service for games that do not expose native telemetry APIs.

It captures a selected game window through the Wayland ScreenCast portal and PipeWire, analyzes user-configured regions, turns visual observations into debounced state transitions, and exposes results through JSON files, a CLI, and local JSON-RPC IPC.

> Status: first usable Linux release verified on CachyOS/Arch with Hyprland. Profile
> schema 1 and protocol 1 provide validated storage, recovery, portable archives,
> daemon/CLI/GUI control, deterministic detectors, replay, and durable/live outputs.

## Intended use cases

- Detect critical health or resource levels.
- Detect victory, defeat, run start, and run end screens.
- Recognize a new level or area name.
- Detect known pets, items, icons, or game modes.
- Feed game events into overlays, automations, stream tooling, or accessibility helpers.

This is a visual observation tool. It does not read game memory, inject into the game, or generate game input.

## Components

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

The binary names and protocol-v1 command names are compatibility-sensitive.

## Linux requirements and source installation

- A Wayland desktop with a working `xdg-desktop-portal` ScreenCast backend.
- PipeWire.
- Rust 1.85 or newer for source builds.
- PipeWire, Wayland, D-Bus, and a working desktop portal development stack.
- Tesseract 5 and Leptonica development libraries for the OCR backend.

On Ubuntu, install the native development dependencies with:

```bash
sudo apt-get update
sudo apt-get install clang libclang-dev libleptonica-dev libpipewire-0.3-dev \
  libspa-0.2-dev libtesseract-dev pkg-config
```

The current reference build is x86-64 CachyOS/Arch Linux with Hyprland,
PipeWire 1.6.6, Wayland client 1.25.0, and Rust 1.95. Install from a checkout:

```bash
./scripts/install-user.sh
systemctl --user enable --now yash-app-eventsd
yash-eventsctl status
yash-app-events
```

This installs user files below `~/.local` and `~/.config`; no root access is used.
Interactive portal selection, restoration, cancellation, and capture are verified on
the documented Hyprland environment. GNOME and KDE are not currently claimed.

## Workflow

1. Start the daemon or let socket activation start it.
2. Open the GUI.
3. Select a game window through the desktop portal.
4. Freeze or inspect the live preview.
5. Draw normalized HUD regions, or select an existing named zone from the zone list above the preview.
6. Assign a detector to each region.
7. Convert observations into temporal event rules.
8. Test the profile against live frames or a replay.
9. Save or export the profile.
10. Consume events from files, the CLI, or JSON-RPC subscriptions.

The profile sidebar includes daemon-backed revision history. Selecting a retained
revision shows a stable-ID comparison; rollback requires confirmation and creates a
new revision, leaving the replaced revision recoverable.

Detection hierarchy supports daemon-owned derived text observations. Selecting a derived
parent exposes its name, enabled state, format placeholders, named detector inputs, live
value, and text event rule; selecting a child opens that detector's tuning controls.

The GUI's **Layout compatibility** panel compares the portable profile reference
resolution/aspect ratio with the current capture, shows X/Y scaling and normalized-zone
behavior, and warns when letterboxing, cropping, UI scale, or aspect mismatch may make a
profile created on another machine target the wrong pixels.

The preview requests a bounded high-detail image up to 1600×900; it never changes the
full-resolution frame used by detectors. The always-visible **Live evidence** panel shows
capture resolution/rates/errors, current observations, event states, daemon/GUI CPU and
resident memory, and the most recent manual detector-test value, confidence, status,
and diagnostic.

Implemented CLI usage:

```bash
yash-eventsctl status
yash-eventsctl profile list
yash-eventsctl profile create "My game" my_game
yash-eventsctl profile validate ./profile.json
yash-eventsctl profile activate <profile-uuid>
yash-eventsctl events follow --json
yash-eventsctl state --json
yash-eventsctl --json replay ./manifest.json
yash-eventsctl --json suite evaluate /path/to/blazblue-entropy-effect
yash-eventsctl collection policy-set <profile-uuid> /path/to/blazblue-entropy-effect --enabled true
yash-eventsctl --json collection auto-review <profile-uuid>
yash-eventsctl collection review <profile-uuid> <item-id> correct --expected ./expected.json
yash-eventsctl collection review <profile-uuid> <item-id> promote --expected ./expected.json
yash-eventsctl diagnostic plan --profile-id <uuid> --element-id <uuid>
```

Profile replay uses the same daemon publication boundary as live processing: durable
state/events, subscriptions, and enabled profile output routes are updated while the
manifest is evaluated. The GUI keeps long image/OCR evaluations bounded to five minutes
and displays the final metrics when processing completes.

The daemon and live commands require `XDG_RUNTIME_DIR`; offline profile validation does
not require a running daemon. All commands accept `--json`, `--socket`, and
`--timeout-ms`.

Post-release detector work adds typed boolean/text rules, Tesseract OCR, deterministic
fixed-layout seven-segment recognition, and portable ONNX classifiers. OCR and classifiers use change-triggered bounded scheduling and the
same image replay and temporal-event path as deterministic detectors. See
`docs/ocr.md`, `docs/classifier.md`, and `docs/diagnostics.md` for current evidence and
limitations.

Real-game regression media can remain outside Git (the repository ignores `/assets/`).
An external suite pins a portable profile and checksummed media, and can mix full
captures, partial screenshots positioned in the profile's reference frame, and exact
zone crops. Run it through `suite evaluate`; a detector or event mismatch returns exit
status 7. See `docs/replay.md` for the package schema and calibration workflow.

The opt-in passive collector can extend such a package only while its game capture is
active. Its machine-local policy defaults to 70 seconds with 10 seconds of jitter,
skips perceptually similar frames when detector evidence is also unchanged, and applies
item/byte quotas. Captures enter an unverified review queue with their exact analyzed
frame, observations, transitions, profile revision/hash, and source metadata. The GUI
and `collection` CLI expose the same protocol operations to inspect, compare, accept,
correct, reject, safely auto-review, and promote items. Ambiguous automation results
remain `needs_correction`; only promoted items become checksummed regression cases.

## Configuration and output

Portable profiles live below the XDG data directory and contain a versioned profile
document plus relative template/model assets. Machine-local capture bindings live
separately below the XDG config directory.

Runtime output provides:

- `events.jsonl`: append-only meaningful state transitions.
- `state.json`: atomically replaced current state snapshot.
- JSON-RPC subscriptions: live events for connected clients.
- Machine-local profile output routes: filtered event/state JSON or raw-text templates delivered to
  append/replace files or direct bounded commands, with GUI enable/test controls.

Routes never travel with portable profiles and command sinks never use a shell. Configure
them with `yash-eventsctl output`, then review, test, and enable them in the GUI or CLI.
See `docs/outputs.md` and `SPECS.md` for schemas and behavior.

Portable profiles may instead carry inert output recipes. The GUI lists these examples,
shows their source hash and disclosed output, supports safe preview/editing, requires the user
to choose an absolute local sink, and installs a new machine-local route disabled. Preview,
delivery testing, and enabling are three separate actions.

## Backup, recovery, upgrade, and uninstall

Stop the daemon before a filesystem backup. Portable profiles are below
`${XDG_DATA_HOME:-~/.local/share}/yash-app-events/profiles`; machine-local capture
bindings are below `${XDG_CONFIG_HOME:-~/.config}/yash-app-events`; events and state
are below `${XDG_STATE_HOME:-~/.local/state}/yash-app-events`. Prefer `profile export`
for portable backups. Trashed profiles can be restored through the GUI or CLI, and
bounded revision history protects earlier committed documents.

To upgrade, pull the desired revision and rerun `scripts/install-user.sh`, then restart
the user service. To uninstall, disable the service and remove the three installed
binaries, desktop/service/icon/completion/man files listed in the install script.
Data is deliberately retained; remove the three `yash-app-events` XDG directories
only after exporting anything you need.

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

The CI-safe replay vertical slice is covered by the daemon test
`synthetic_health_replay_reaches_files_state_and_live_subscription`; it asserts that
the same two transitions appear in `events.jsonl`, atomic `state.json`, `state.get`,
and a live protocol subscription.

Interactive Wayland capture is verified on the documented Hyprland environment. The
daemon owns capture; `yash-eventsctl capture select` opens the picker, `capture status`
reports metrics, `capture snapshot <path>` explicitly saves one PNG, and `capture
stop` releases the session. See `docs/capture-smoke.md` for the acceptance procedure
and the current GNOME/KDE compatibility boundary.

## Project principles

- Linux/Wayland first, with replaceable capture backends.
- Raw continuous capture, but bounded 5–10 FPS analysis by default.
- Latest frame wins; stale frames are dropped.
- Deterministic vision before OCR or neural inference.
- Observations are not events; temporal rules produce events.
- Portable, versioned, recoverable configuration.
- One daemon and one protocol for GUI, CLI, and integrations.
- Inspectable outputs and replay-based verification.

## License

Licensed under either of the Apache License, Version 2.0 or the MIT license, at your
option. See `LICENSE-APACHE` and `LICENSE-MIT`.
