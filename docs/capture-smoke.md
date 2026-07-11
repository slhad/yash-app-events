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

Required release evidence is successful selection and cancellation on the documented
Hyprland backend. GNOME and KDE are compatibility candidates rather than release
requirements. CI runs the noninteractive format/stride/error/metrics tests only; it
must not claim interactive portal permission evidence.

## Recorded Hyprland evidence

On 2026-07-11, CachyOS/Hyprland with PipeWire 1.6.6 selected a monitor through the
ScreenCast portal and delivered a 3840×2160 packed frame with 15,360-byte stride.
The backend normalized the negotiated BGRx-family source to `Rgba8`; after one second
the smoke reported two input frames, one latest-frame replacement, no error, and then
released the stream/session promptly. The daemon-owned path subsequently reported 31
frames, 30 replacements, 93 ms frame age, and no error, and an explicitly requested
4K PNG snapshot completed atomically with a 30-second debug-build CLI timeout. That
temporary snapshot was inspected for normalized HUD regions and deleted after use.

Selection and successful frame flow are therefore verified on Hyprland. A later
ExplicitlyRevoked selection stored a restore token outside the portable profile. After
stopping capture, `capture select --profile-id` restored the approved 3840×2160 source
as node 160 in 35 ms, reported `restore_token_saved: true`, and resumed packed RGBA
frames without opening a picker. The token itself is deliberately excluded from this
report, logs, and exported profiles.

The backend's persisted permission caused a nominal no-token smoke to immediately reuse
the approved display. Cancellation was therefore tested on an isolated session bus and
temporary permission database while retaining the real Hyprland and PipeWire session.
Pressing Escape in the fresh chooser made `xdg-desktop-portal-hyprland` report its
backend-specific `Invalid session`; the capture backend normalized that known response
to `CaptureError::Cancelled`. The readiness-channel race that previously masked this as
`SessionEnded` is fixed. Shared OBS and desktop permissions were never modified.

The Hyprland chooser exposes selection and cancellation but no separate denial button.
Policy denial is verified through ashpd's typed `PortalError::NotAllowed` mapping and
unit test. This satisfies denial behavior without fabricating interactive UI evidence.

## Isolated KDE attempt

The reference host also has Plasma/KWin 6.6.5 and `xdg-desktop-portal-kde` 6.6.5.
An isolated session bus successfully started a 1280×720 virtual KWin Wayland
compositor, activated the KDE portal backend, and routed ScreenCast to it without
touching the live Hyprland session. KDE rejected the virtual framebuffer before its
chooser with portal response `Other`; this is not counted as successful KDE evidence.
A real Plasma login with a physical or DRM-backed output would still be required before
claiming KDE compatibility; KDE is not part of the current supported environment.

Cancellation and denial classification uses ashpd's typed `ResponseError::Cancelled`,
`PortalError::Cancelled`, and `PortalError::NotAllowed` variants. Backend-specific
English-message matching remains only as a compatibility fallback and is covered by
unit tests.
