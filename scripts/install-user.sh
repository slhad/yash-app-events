#!/usr/bin/env bash
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
prefix=${PREFIX:-"$HOME/.local"}
config_home=${XDG_CONFIG_HOME:-"$HOME/.config"}
data_home=${XDG_DATA_HOME:-"$HOME/.local/share"}

cargo build --manifest-path "$root/Cargo.toml" --release --locked --workspace
install -Dm755 "$root/target/release/yash-app-eventsd" "$prefix/bin/yash-app-eventsd"
install -Dm755 "$root/target/release/yash-app-events" "$prefix/bin/yash-app-events"
install -Dm755 "$root/target/release/yash-eventsctl" "$prefix/bin/yash-eventsctl"
install -Dm644 "$root/packaging/systemd/yash-app-eventsd.service" "$config_home/systemd/user/yash-app-eventsd.service"
install -Dm644 "$root/packaging/applications/io.github.yash_app_events.desktop" "$data_home/applications/io.github.yash_app_events.desktop"
install -Dm644 "$root/packaging/icons/io.github.yash_app_events.svg" "$data_home/icons/hicolor/scalable/apps/io.github.yash_app_events.svg"
install -Dm644 "$root/packaging/completions/yash-eventsctl.bash" "$prefix/share/bash-completion/completions/yash-eventsctl"
install -Dm644 "$root/packaging/man/yash-app-eventsd.1" "$prefix/share/man/man1/yash-app-eventsd.1"
install -Dm644 "$root/packaging/man/yash-eventsctl.1" "$prefix/share/man/man1/yash-eventsctl.1"
if [[ ${YASH_SKIP_SYSTEMD_RELOAD:-0} != 1 ]]; then
  systemctl --user daemon-reload
fi
printf 'Installed under %s. Start with: systemctl --user enable --now yash-app-eventsd\n' "$prefix"
