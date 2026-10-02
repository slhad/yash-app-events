#!/usr/bin/env bash
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
config_home=${XDG_CONFIG_HOME:-"$HOME/.config"}
plugin_id=io.github.yash-app-events.status
destination="$config_home/omarchy/plugins/$plugin_id"

if [[ $# -gt 1 || ( $# -eq 1 && $1 != --enable ) ]]; then
  printf 'Usage: %s [--enable]\n' "$0" >&2
  exit 2
fi
if [[ ${1:-} == --enable ]] && ! command -v omarchy >/dev/null; then
  printf 'Enabling requires the Omarchy shell and omarchy CLI.\n' >&2
  exit 1
fi
if [[ -d $destination ]]; then
  backup=$(mktemp -d "$config_home/omarchy/yash-status-backup.XXXXXX")
  cp -a -- "$destination" "$backup/"
  printf 'Previous plugin backed up to %s\n' "$backup"
fi
for file in manifest.json BarWidget.qml StatusService.qml Status.js yash-events.svg; do
  install -Dm644 "$root/integrations/quickshell/$file" "$destination/$file"
done
if command -v omarchy >/dev/null; then
  omarchy plugin validate "$destination"
fi
if [[ ${1:-} == --enable ]]; then
  if [[ -f $config_home/omarchy/shell.json ]]; then
    backup=$(mktemp "$config_home/omarchy/shell.json.yash-backup.XXXXXX")
    cp -p -- "$config_home/omarchy/shell.json" "$backup"
    printf 'Shell settings backed up to %s\n' "$backup"
  fi
  # File watching is asynchronous; discover the new manifest before placing it.
  omarchy-shell shell rescanPlugins
  omarchy bar put "$plugin_id" --section right
fi
printf 'Installed %s. Enable with: omarchy bar put %s --section right\n' "$destination" "$plugin_id"
