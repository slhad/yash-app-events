#!/usr/bin/env bash
set -euo pipefail

if ! grep -q '^> Status: first usable Linux release verified on CachyOS/Arch with Hyprland' README.md; then
  echo 'README must accurately identify the currently verified implementation level.' >&2
  exit 1
fi

if grep -Eq 'GNOME and KDE are (currently )?supported|verified on .*GNOME|verified on .*KDE' README.md; then
  echo 'README must not claim unverified GNOME or KDE portal support.' >&2
  exit 1
fi
