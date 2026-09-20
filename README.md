# yash-app-events

`yash-app-events` is a Linux-first HUD observation and event service for games that do not expose native telemetry APIs.

It captures a selected game window through the Wayland ScreenCast portal and PipeWire, analyzes user-configured regions, turns visual observations into debounced state transitions, and exposes results through JSON files, a CLI, and local JSON-RPC IPC.

> Status: first usable Linux release verified on CachyOS/Arch with Hyprland.
> Profile schema 2 and protocol 1 provide validated storage, recovery, portable archives,
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
  libspa-0.2-dev libtesseract-dev pkg-config tesseract-ocr-eng
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
7. Group anchors and contextual detectors into scenes and overlays when the game has multiple screens.
8. Author named interaction rectangles and optional safe points; these are descriptive and never generate input.
9. Convert observations into temporal event rules.
10. Test the profile against live frames, one PNG, or a replay.
11. Save or export the profile.
12. Consume scene context and events from files, the CLI, or JSON-RPC subscriptions.

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

The region canvas has an explicit **Preview scope** selector. Multi-screen profiles open
on their first scene instead of painting every configured rectangle at once. Choose one
scene or overlay to see its global regions, recognition anchors, contextual detectors,
visibility evidence, and owned interaction targets; use **Global + all recognition
anchors** or **All layers** for deliberate cross-layer review. A detector selected in the
zone list remains visible while it is edited even when it is outside the current scope.

The preview requests a bounded high-detail image up to 1600×900; it never changes the
full-resolution frame used by detectors. The always-visible **Live evidence** panel shows
capture resolution/rates/errors, current observations, event states, daemon/GUI CPU and
resident memory, and the most recent manual detector-test value, confidence, status,
and diagnostic.

The **Profile capacity** panel shows detector, scene, overlay, layer-reference, recognition,
and interaction-target usage before a commit fails. Drawing, duplication, layer membership,
and scene/overlay authoring use a read-only projection first, so an over-limit action leaves
the draft unchanged and reports the exact resource and projected count. Removing a region
also removes dependent derived observations, rules, recognition anchors, and target-visibility
references from the draft so capacity can be recovered without creating dangling IDs. The panel
also reports the always-evaluated anchor count and weighted detector cost, exposing runtime work
hidden behind a scene's local element count.

For large profiles, prefer these bounded consolidation patterns in order: reuse shared global
or scene anchors, put rarely needed detectors in contextual layers, compose text with derived
observations, combine stable alternatives in one multi-template detector, and remove unreachable
or exact-duplicate regions. Use a profile bundle when unrelated HUD families need separate
profiles: a small router profile recognizes the screen family, then `analyze` evaluates only one
independently validated member. Keep the router and every member below 512 elements, preferably
below 450 to leave authoring headroom. The capacity report is advisory; it does not rewrite stable
IDs automatically.

When equivalent detector elements are consolidated, schema-2 profiles may retain
`element_aliases` so older suite and integration names continue resolving to the canonical
stable element ID. `profile pack` exports the current profile's declared detector assets and
omits stale template files without deleting anything from the source directory.

Removing a region also removes its dependent references. Scenes and overlays that lose
recognition evidence are disabled, and affected interaction targets are hidden until their
visibility conditions are repaired in the draft.

For schema-2 profiles with scenes or overlays, the daemon retains and prewarms only the global
and enabled-layer recognition-anchor detectors. Contextual detector configurations are kept as lightweight
slots, constructed when their layer becomes active, and released when it becomes inactive.
This preserves the existing anchor-first JSON/RPC behavior while bounding resident OCR,
classifier, and decoded-template state; profiles without a scene model retain eager behavior.

Implemented CLI usage:

```bash
yash-eventsctl status
yash-eventsctl profile list
yash-eventsctl profile create "My game" my_game
yash-eventsctl profile validate ./profile.json
yash-eventsctl profile pack ./portable-profile ./portable-profile.hudprofile
yash-eventsctl --json profile bundle validate ./queen.profile-bundle.json
yash-eventsctl profile activate <profile-uuid>
yash-eventsctl events follow --json
yash-eventsctl state --json
yash-eventsctl --json analyze /path/to/frame.png --profile-id <profile-uuid>
yash-eventsctl --json analyze /path/to/frame.png --profile-bundle /path/to/queen.profile-bundle.json
yash-eventsctl --json analyze /path/to/frame.png --profile-id <profile-uuid> \
  --context-json '{"expected_scene_names":["guild_battle_opponent"]}'
yash-eventsctl --json replay ./manifest.json
yash-eventsctl --json suite evaluate /path/to/blazblue-entropy-effect
yash-eventsctl --json suite evaluate /path/to/package --case exact-case-id --timings
yash-eventsctl --json profile analyze-capacity /path/to/profile.json
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

The daemon and live commands require `XDG_RUNTIME_DIR`; offline profile validation and
packing do not require a running daemon. `profile pack` validates a portable profile
directory and builds the same inert `.hudprofile` archive accepted by daemon import.
All commands accept `--json`, `--socket`, and `--timeout-ms`.

`profile analyze-capacity` is read-only. It compares template assets by content when the profile
directory is available and reports always-evaluated recognition anchors. If the only validation problem is that a profile already
contains more than 512 detector elements, the command still produces a diagnostic so the author
can identify unreachable, duplicate, or consolidation candidates. That analysis-only path never
admits the profile to normal load, save, pack, or activation.

Schema-2 profiles recognize one base scene plus independent overlays through bounded
N-of-M anchor evidence. Live analysis runs global/anchor detectors first and gates
contextual detectors to active layers. `state` includes the current context, and scene
changes are emitted as `scene_changed` events.

The one-shot `analyze` command is JSON-first: it returns exact frame dimensions and
SHA-256, scene candidates, named observations, normalized and pixel rectangles, and
profile-authored interaction targets with optional safe points. It never embeds image
bytes. `ai_handoff.json_sufficient` depends only on explicitly required observations;
otherwise the result recommends at most eight minimal named crops. OCR fields may also
declare a bounded retry hint and expected format; malformed or transiently occluded text
then appears under `ai_handoff.retry` with a delay and maximum attempt count. Full-frame review is
reserved for unknown scenes or broad layout/aspect incompatibility. Profiles that need
pixel-exact execution can opt into strict reference dimensions while existing normalized
profiles remain aspect-compatible across resolutions.

When a game needs more than one independently maintained HUD family, `analyze` also accepts
`--profile-bundle` (or its `--bundle` alias). A bundle is a bounded JSON routing document with
one router profile, pinned profile revisions, a shared game slug, and members selected by the
router's stable scene/overlay IDs. Member detector assets are initialized and evaluated only after
a route wins by priority and specificity; only that member is loaded for the second stage. An
unresolved or unmatched route falls back to the optional fallback member, otherwise the router
result is returned with `profile_selection.status` set to
`unresolved` or `no_match`. The bundle path contains IDs only; profile directories and assets are
still owned by the daemon's profile store. `profile bundle validate` checks the document offline,
while the daemon checks the router and selected member's revisions, game, layout, and router
scene/overlay IDs. Bundle analysis remains pure and never activates a profile or publishes an
event.

For consecutive one-shot frames, `--context-json` can carry the previous recognized
profile/scene/overlay IDs and optional expected scene/overlay names or stable IDs for the next
frame. Names are resolved against the selected profile, which lets a controller describe a
reviewed destination without hard-coding profile-specific UUIDs. Yash uses this only to rank
equal-confidence candidates; it always keeps full-profile detection and reports whether the
context was applied or ignored. The context contains no image bytes, coordinates, or permission
to act, so callers can safely fall back to ordinary profile analysis when a transition is
unexpected.

Example bundle shape:

```json
{
  "schema": 1,
  "name": "Queen Blade control",
  "game": "queens_blade_limit_break",
  "router_profile_id": "00000000-0000-0000-0000-000000000001",
  "router_profile_revision": 3,
  "members": [
    {
      "profile_id": "00000000-0000-0000-0000-000000000002",
      "profile_revision": 12,
      "scene_ids": ["00000000-0000-0000-0000-000000000011"],
      "overlay_ids": [],
      "priority": 100,
      "fallback": false
    },
    {
      "profile_id": "00000000-0000-0000-0000-000000000003",
      "profile_revision": 8,
      "scene_ids": [],
      "overlay_ids": [],
      "priority": 0,
      "fallback": true
    }
  ]
}
```

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
- `state.json`: atomically replaced current state snapshot, published once per analyzed frame.
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

## Public Profile Catalog

The GUI's collapsed **Profile Catalog** browser and the shared CLI can fetch reviewed
portable profiles from the repository's single rolling GitHub release tagged `profiles`:

```bash
yash-eventsctl catalog status
yash-eventsctl catalog refresh
yash-eventsctl catalog list
yash-eventsctl catalog install <catalog-id> <version> \
  --catalog-revision <reviewed-revision> --sha256 <reviewed-package-sha256>
```

The daemon performs all network, cache, integrity, and profile writes. It contacts only the
fixed `slhad/yash-app-events` profile release after an explicit browse/refresh/install action,
keeps the last valid catalog atomically below the XDG cache directory for offline browsing,
and verifies the declared size and SHA-256 before using the normal hardened profile importer.
Installation never activates the profile or authorizes its inert output recipes.

Catalog sources are reviewable under `catalog/profiles/`; generated `.hudprofile` packages
and catalog indexes live only on the release. The initial BlazBlue Entropy Effect Stage
Tracker contains OCR/seven-segment configuration and two raw-text output recipes, but no
gameplay image, video, thumbnail, capture binding, local route, or restore token. See
`catalog/README.md` for the append-only publication layout.

## Backup, recovery, upgrade, and uninstall

Stop the daemon before a filesystem backup. Portable profiles are below
`${XDG_DATA_HOME:-~/.local/share}/yash-app-events/profiles`; machine-local capture
bindings are below `${XDG_CONFIG_HOME:-~/.config}/yash-app-events`; events and state
are below `${XDG_STATE_HOME:-~/.local/state}/yash-app-events`. Prefer `profile export`
for portable backups. Trashed profiles can be restored through the GUI or CLI, and
bounded revision history protects earlier committed documents. The daemon also keeps
an atomic machine-local revision high-water file beside the profiles. It is not part
of exports. A fresh profile ID keeps the archive revision, while an import for a
previously seen ID is rebased to the next known revision, including after trash,
permanent deletion, or a manually named profile backup. This prevents an import from
making a profile appear to jump backward to revision 0.

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

Replay decodes one frame at a time. Image analysis and preview encoding use bounded
background workers so control requests remain responsive. GUI polling coalesces pending
requests during long operations. See [performance evidence](docs/performance.md) for the
audit, workload limits, and reproducible publication, capture, and detector benchmarks.

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
