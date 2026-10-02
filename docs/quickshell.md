# Quickshell status integration

The optional `io.github.yash-app-events.status` plugin adds a persistent Yash Events
indicator to the Omarchy Quickshell bar. Capture and analysis remain daemon-owned.
It uses protocol-1 JSON-RPC directly, with no CLI subprocess on each refresh.

## Installation

Install the application normally, then install and enable the widget from the checkout:

```bash
./scripts/install-quickshell.sh --enable
```

This requires the Omarchy shell running on Quickshell. Plain Quickshell users can
reuse `StatusService.qml` and `Status.js` in their own shell; `BarWidget.qml` uses
Omarchy's `qs.Ui` components. The integration is separate from `install-user.sh`
so other desktops do not receive Omarchy configuration.

The installer respects `XDG_CONFIG_HOME`, copies the plugin to
`${XDG_CONFIG_HOME:-~/.config}/omarchy/plugins/io.github.yash-app-events.status`,
validates it when the Omarchy CLI is available, and adds it to the right section
only with `--enable`. Omarchy hot-reloads plugin code and bar settings. Existing
plugin files and shell settings are backed up before replacement or enablement.
Rerun the installer after updating the checkout.

If the installed files are current but the bar still shows the previous widget,
request a plugin rescan. If it remains stale, restart the shell from a terminal in
the active desktop session:

```bash
omarchy-shell shell rescanPlugins
omarchy restart shell
```

The restart briefly recreates the bar and shell panels. It preserves shell settings
and does not restart the Yash daemon. A plugin-reload log alone does not prove that
the displayed component changed; check the actual bar after updating it.

## Display and actions

| Bar state | Meaning |
| --- | --- |
| Offline | No negotiated daemon connection, or a reply timed out/failed validation |
| Idle | Daemon reachable, capture stopped, no active profile |
| Stopped | Active profile selected, capture stopped |
| Capturing | Capture active with no profile, or an older daemon lacks freshness metrics |
| Waiting | Capture and profile active, no analysis has completed in this session |
| Processing | Capture and profile active, analysis completed within five seconds |
| Stalled | Capture and profile active, latest completed analysis is older than five seconds |
| Error | Current capture or output failure |

The bar uses a custom vector HUD icon in one fixed icon slot on horizontal and vertical
bars. Its status dot is green for Processing, blue for Capturing, amber for Waiting,
and the shell's alert color for Stalled or Error. Idle, Stopped, and Offline are dimmed.
The tooltip includes the profile name only while capture is active, plus selected source,
capture state, input/analysis session-average FPS, analysis age, replaced frames, and
historical detector-error count. Stopped capture hides the selected profile.

Left click launches `yash-app-events`. Right click refreshes status and the cached
profile name while capture is active, including after a rename. Stopping capture,
a profile change, or daemon reconnect clears the cached name automatically. During
capture, the stable profile ID appears until lookup succeeds.
The plugin never starts capture, activates profiles, or enables output routes.

## Settings and removal

Use Omarchy's settings UI or CLI:

```bash
omarchy bar set io.github.yash-app-events.status guiPath /absolute/path/to/yash-app-events
omarchy bar set io.github.yash-app-events.status socketPath /absolute/path/to/control.sock
omarchy plugin disable io.github.yash-app-events.status
```

`socketPath` defaults to `YASH_APP_EVENTS_SOCKET` if set in the shell environment,
otherwise `$XDG_RUNTIME_DIR/yash-app-events/control.sock`. With neither a configured
path nor `XDG_RUNTIME_DIR`, the widget remains offline and explains the missing setting.
`guiPath` is a single executable, invoked directly without a shell or extra arguments.
Use an absolute path if the shell's PATH does not include the application.

After disabling the plugin, remove its directory to uninstall it. Disable/remove
does not affect the daemon, profiles, capture bindings, or outputs.

## Protocol and resource limits

Each widget instance holds one Unix socket. It negotiates before polling
`system.status` every two seconds and requests `profile.get` only during capture, on
identity changes or explicit refresh. There is at most one outstanding request, with a five-second
deadline. Disconnect, invalid reply, or timeout clears the displayed status; retries
are spaced by two seconds. Complete replies over 1 MiB are rejected. The local daemon
is the trusted transport peer; Quickshell's newline parser buffers until a delimiter.
No subscription/frame/history queue or image transport is used.

`last_analysis_age_ms` is a monotonic age at sampling time; the widget adds elapsed
time between polls. It never infers current processing from average FPS. Daemons
without this additive field show Capturing with an upgrade explanation. The five-second
freshness window describes recent live analysis, not whether a detector thread is
executing at that instant. One-shot analysis and offline suites are outside this display.

## Verification

```bash
node --test integrations/quickshell/tests/*.test.cjs
qmllint -I /usr/lib/qt6/qml integrations/quickshell/StatusService.qml
omarchy plugin validate integrations/quickshell
python3 scripts/test-quickshell-smoke.py
```

The native smoke requires Quickshell 0.3.1 and Python 3. It uses an offscreen QML
service and a temporary Unix socket, leaving the physical desktop untouched. It
checks all eight states, fragmented framing, timeout, malformed replies, and reconnect
after a simulated daemon restart. Node tests need no Quickshell or running daemon.
The standard workspace checks also verify status compatibility and live freshness.

Dated local acceptance evidence is recorded in
[quickshell-acceptance.md](quickshell-acceptance.md).
