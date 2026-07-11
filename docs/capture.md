# Linux capture implementation decision

The Linux backend uses `ashpd` 0.12 and direct `pipewire-rs` 0.10. The portal owns
source selection and returns a restricted PipeWire remote plus node ID. A dedicated
PipeWire thread negotiates only RGB, RGBA, or RGBx and publishes copied CPU frames to
the bounded latest-frame slot. Its process callback never waits for detection or I/O.
A PipeWire loop channel makes daemon stop independent of frame arrival.

Direct PipeWire was selected over GStreamer because the maintained binding example
already covers the portal FD/node flow, the installed development library is usable,
and the implementation needs only packed RGB formats. This avoids a second media
pipeline and conversion policy while retaining the backend-neutral `Frame` boundary.
Unsupported formats fail visibly rather than being interpreted incorrectly.

Automated development-host context recorded 2026-07-11:

- Wayland / Hyprland (Omarchy)
- `xdg-desktop-portal` 1.20.4
- `xdg-desktop-portal-hyprland` 1.3.12
- PipeWire 1.6.6

These facts prove dependency availability, not successful permission interaction.
Interactive evidence is tracked separately by `docs/capture-smoke.md`.
