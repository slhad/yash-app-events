# Native GUI acceptance report

Tested 2026-07-11 on CachyOS/Omarchy, Hyprland, PipeWire 1.6.6, a 3840×2160
BlazBlue Entropy Effect portal source, and the real protocol-v1 daemon. The native
egui window was driven with `ydotool`/Hyprland input and captured with `grim`.

Local evidence (intentionally ignored by Git because it contains copyrighted gameplay):

- `artifacts/gui-acceptance/01-live-preview.png` — active opt-in preview with all five
  normalized regions over a live frame and capture/replacement metrics.
- `artifacts/gui-acceptance/05-zones-runtime-live.png` — explicit five-zone selector and
  always-visible capture/observation/event/detector evidence panel against live 4K capture.
- `artifacts/gui-acceptance/02-frozen-detector.png` — connection-scoped frozen frame,
  ordinary-click health selection, detector-specific form, and temporal rule editor.
- `artifacts/gui-acceptance/03-replay-evaluation.png` — manifest evaluation reporting
  precision/recall 1.000, no duplicates/misses, zero latency, and PASS.
- `artifacts/gui-acceptance/fixtures/blazblue-gameplay.png` — explicitly authorized
  3840×2160 portal snapshot.
- `artifacts/gui-acceptance/fixtures/blazblue-gameplay-10s.mp4` — explicitly authorized
  3840×2160 60 FPS recording; 9.7 seconds of encoded media from a ten-second wall-clock capture.
- `artifacts/gui-acceptance/fixtures/blazblue-preview-320x180.png` — bounded preview fixture.

## Verified workflows

1. GUI loads the daemon-owned revision-2 BlazBlue profile over JSON-RPC.
2. Preview is opt-in, bounded to 320×180 transport, decoded off the render thread,
   displayed aspect-correctly, and does not affect the detector input path.
3. Preview and freeze leases are connection-scoped. After an induced timeout, the GUI
   reconnects and automatically restores desired preview/freeze state.
4. A normal click selects the topmost normalized region and opens its detector/rule editor.
5. Existing regions are also selectable by their name and detector type in the zone list,
   without hit-testing overlapping rectangles on the canvas.
6. `preview.frame` accepted a 1600×900 bounded request against a 3840×2160 live frame and
   returned a 1600×900 PNG (1,166,020 encoded bytes in this acceptance run).
7. A draft-enabled region-change element tested over protocol-v1 on a frozen live frame;
   it returned two lifecycle samples and ended `valid`, value `0.0`, confidence `1.0`, with
   diagnostic `change 0.0000; stable=true`.
5. Frozen detector testing succeeds programmatically on the exact captured frame in
   55 ms, returning a valid observation and a bounded 481×15 diagnostic PNG.
6. Replay fixtures now use the detector's configured color/direction and the full
   element-crop extent. The real BlazBlue rule emits `entered` at 300 ms and `left`
   at 1300 ms after its 1000 ms cooldown.
7. Color-bar confidence represents classification certainty rather than fill level;
   a confidently measured 10% bar retains confidence 1.0.

## Defects found and fixed

- BGR/BGRA/BGRx were absent from PipeWire negotiation, causing Hyprland's live stream
  to report no compatible formats.
- Diagnostic crops above 512 pixels were rejected instead of downscaled.
- Canvas selection required a drag; ordinary click selection now works and is tested.
- Replay colors were hard-coded red and replay crops were cropped a second time.
- Color-bar confidence incorrectly equaled fill percentage, suppressing low-health rules.
- Preview/freeze leases were not restored after worker reconnect.
- Successful RPC/preview responses did not clear stale GUI error messages.
- When the GUI's initial profile request ran before the daemon was available, later
  status reconnects succeeded but never reloaded the profile list. Every successful
  reconnect now refreshes profiles before continuing; the installed GUI was launched
  with the daemon stopped, then recovered revision 3 and all five zones after daemon
  startup without restarting the GUI (`09-daemon-late-reconnect.png`).
- GUI source selection inherited the three-second ordinary RPC deadline. The portal
  remained interactive after the GUI worker disconnected, leading to a late broken
  pipe and a misleading compositor-looking failure. `capture.select` now waits up to
  five minutes on the background worker, disables duplicate selection, and visibly
  reports that it is waiting for the portal.

## GUI portal-selection retest

The installed GUI initiated `capture.select` through protocol-v1 and remained connected
for the complete portal interaction. The restored Hyprland selection became active as
PipeWire node 199 at 3840×2160, approximately 59.8 input FPS and 9.2 analysis FPS; the
GUI process remained alive and the daemon log contained no broken pipe or Wayland
protocol failure. Cancellation/denial acceptance and a second independent portal backend
remain outstanding for the release-wide capture requirements.
