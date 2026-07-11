#!/usr/bin/env bash
set -euo pipefail

if ! grep -q '^> Status: first usable Linux release verified on CachyOS/Arch with Hyprland' README.md; then
  echo 'README must accurately identify the currently verified implementation level.' >&2
  exit 1
fi
if grep -Eq 'interactive support is not yet claimed|Portable profiles will live|Runtime output will provide' README.md; then
  echo "README contains obsolete planned or unverified interactive-support wording" >&2
  exit 1
fi
if ! grep -q 'Interactive Wayland capture is verified on the documented Hyprland environment' README.md; then
  echo "README must retain the verified interactive Hyprland capture statement" >&2
  exit 1
fi

if grep -Eq 'GNOME and KDE are (currently )?supported|verified on .*GNOME|verified on .*KDE' README.md; then
  echo 'README must not claim unverified GNOME or KDE portal support.' >&2
  exit 1
fi
