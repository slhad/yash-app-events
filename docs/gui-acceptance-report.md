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

## Remaining issue

Invoking the interactive portal picker from inside the native GUI produced a Hyprland
Wayland protocol failure (`wl_display: invalid object 7`) after selection and terminated
the GUI. Starting capture through the CLI/JSON-RPC client and then using GUI preview is
stable. This remains release-blocking for `SPEC-UI-003`; it needs a separately
reproducible compositor/parent-window investigation rather than being hidden by the
successful preview evidence.
