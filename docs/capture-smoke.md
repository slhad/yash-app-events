# Interactive Wayland portal capture smoke test

This test is intentionally opt-in because it opens the desktop source picker. It
persists no frame and excludes the cursor by default.

Prerequisites are a Wayland session, a ScreenCast-capable `xdg-desktop-portal`
backend, PipeWire, and `XDG_RUNTIME_DIR`. Run:

```bash
cargo run -p yash-app-events-capture-pw --example portal_smoke
```

Select a non-sensitive test window. The program must report a packed RGB/RGBA frame,
nonzero input FPS, replacement counts, resolution, format, and then `capture stopped`
within one second after its measurement. While it runs, confirm the compositor shows
its capture indicator. Confirm no screenshot appeared below XDG data/state/cache.

Repeat and cancel the picker; the error must say selection was cancelled. If the
backend offers a deny action, deny it and confirm the error identifies permission
denial. Run twice with a profile through `yash-eventsctl capture select --profile-id`
to confirm restoration is attempted and a stale token falls back to a picker.

Record these commands with their package versions for each tested environment:

```bash
echo "$XDG_CURRENT_DESKTOP $XDG_SESSION_TYPE"
pipewire --version
systemctl --user --no-pager status xdg-desktop-portal.service
systemctl --user --no-pager status xdg-desktop-portal-hyprland.service
systemctl --user --no-pager status xdg-desktop-portal-gnome.service
```

Required release evidence is one successful window selection on the documented
Hyprland backend and one on an independently implemented backend such as GNOME or
KDE. CI runs the noninteractive format/stride/error/metrics tests only; it must not
claim portal permission evidence.

## Recorded Hyprland evidence

On 2026-07-11, CachyOS/Hyprland with PipeWire 1.6.6 selected a monitor through the
ScreenCast portal and delivered a 3840×2160 packed frame with 15,360-byte stride.
The backend normalized the negotiated BGRx-family source to `Rgba8`; after one second
the smoke reported two input frames, one latest-frame replacement, no error, and then
released the stream/session promptly. The daemon-owned path subsequently reported 31
frames, 30 replacements, 93 ms frame age, and no error, and an explicitly requested
4K PNG snapshot completed atomically with a 30-second debug-build CLI timeout. That
temporary snapshot was inspected for normalized HUD regions and deleted after use.

Selection and successful frame flow are therefore verified on Hyprland. Cancellation,
denial, restore-token behavior (this backend returned no token), and an independent
GNOME/KDE portal remain required release evidence.
