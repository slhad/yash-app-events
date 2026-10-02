# Quickshell acceptance, 2026-10-02

Scope: `SPEC-OBS-004`, additive status fields under `SPEC-OBS-002`, and the existing
local transport, handshake, and capture-visibility boundaries.

The initial evidence below covers plugin 0.1.0. The compact-icon follow-up for
0.1.1 is recorded separately at the end.

Environment: CachyOS/Arch Linux, Hyprland, Omarchy shell, Quickshell 0.3.1, Qt 6,
Node 22, Python 3, and the repository's installed daemon. These observations are
dated evidence for this environment; they do not claim other Quickshell hosts or
GNOME/KDE integration.

## Automated evidence

- `node --test integrations/quickshell/tests/*.test.cjs`: ten tests pass. They cover
  every display state, stale analysis with positive historical FPS, old payloads,
  capture/output failures, inert bounded text, stable-ID profile changes, one pending
  request, handshake order, timeout, disconnect, and invalid replies. Installer tests
  check XDG destinations, explicit enablement, discovery rescan, and backups.
- `python3 scripts/test-quickshell-smoke.py`: the actual QML service loads offscreen
  and displays all eight states from a synthetic Unix-socket daemon. Fragmented
  newline-framed responses parse correctly. Disconnect, a five-second response
  timeout, and malformed JSON clear status; four negotiated connections end with
  Processing from the recovered daemon.
- `qmllint -I /usr/lib/qt6/qml integrations/quickshell/StatusService.qml` and
  `omarchy plugin validate integrations/quickshell`: pass.
- Protocol tests deserialize old status payloads with null freshness/error defaults
  and round-trip the additive fields. Daemon socket tests expose both fields before
  processing. The live latest-frame worker test proves a recent completion age;
  monotonic-age tests distinguish unstarted, fresh, and thirty-second-old analysis.
- Workspace formatting, strict Clippy, all-feature tests, Rust documentation, README
  claims, JSON parsing, and relative documentation links pass.

## Local installation evidence

`scripts/install-quickshell.sh --enable` installs the plugin under the user's XDG
configuration directory, validates the manifest, rescans Omarchy discovery, and adds
one widget to the right bar section. A normalized comparison with the installer
backup proves that every pre-existing shell setting is retained. The Omarchy registry
reports the widget enabled; shell logs contain no Yash QML loading errors. Each of
the three monitor widgets maintains its own daemon connection.

The release daemon was rebuilt and atomically installed after checking that capture
was stopped. Its previous executable was backed up before restarting the user service.
The running service exposes `last_analysis_age_ms: null` and `capture_error: null`.
An offscreen copy of the same QML service, connected to that real control socket,
displays Stopped with the active Queen Blade portrait profile name and zero analysis FPS.

## Limits of this run

Fresh portal selection and real-game capture were not started. Fresh-processing and
error displays are covered by native synthetic RPC and live-engine tests, while the
real-daemon check covers the existing stopped profile. Left-click GUI launch is a
direct executable call; interactive opening and tooltip geometry were not separately
automated. One-shot screenshot analysis and suite operations are outside this widget's
live-capture scope. No game images were captured or retained.

## Compact icon follow-up, 2026-10-02

Plugin 0.1.1 replaces the bar text with a custom SVG HUD/heartbeat icon in Omarchy's
fixed `BarIconButton` slot. Processing, Capturing, and Waiting use green, blue, and
amber status dots; errors and stalls use the theme's alert color. Idle, Stopped, and
Offline are dimmed. Profile names appear only in the tooltip while capture is active.
Stopping capture clears the profile cache and suppresses profile-name requests;
restarting with the same stable ID performs a new lookup.

All twelve JavaScript/installer tests pass, including stopped-profile hiding during
output errors, same-ID restart, and installation of the SVG resource. The native smoke
loads the SVG through QtQuick Image, verifies stopped/idle/offline views contain no
profile, and repeats all eight states plus framing, timeout, and reconnect checks.
Workspace formatting, strict Clippy, all-feature tests, Rust documentation, manifest
and QML validation, README claims, JSON/XML parsing, and relative links pass.

The installed plugin was updated through the optional installer, preserving bar
placement. Omarchy reported hot-reload attempts without Yash QML or image-loading errors. No new
portal session or game capture was started for this presentation change.

## Running-bar verification correction, 2026-10-02

The user reported that the physical bar still displayed the earlier widget. The
preceding compact-icon checks verified installed files, offscreen loading, and reload
logs, but did not establish that the running bar had replaced its component.

Installed files matched 0.1.1. A plugin rescan was requested, followed by a full
Omarchy shell restart. The first restart encountered an obsolete Hyprland instance
identifier inherited by the agent terminal; relaunching with the active user-session
environment restored the shell. The new shell responds to IPC and its
`debugBarGeometry` reports a visible 27-by-26-pixel Yash widget on each of three
monitors. Temporary crops of all three bars visually confirm the custom HUD icon
and absence of the previous name/profile text. Those screenshots were deleted after
inspection. The Yash daemon and capture configuration were not restarted or changed.
