#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/.." && pwd)
install_root=${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}
installed_binary="$install_root/bin/raylander"
daemon_was_running=false
pgrep -x raylander >/dev/null 2>&1 && daemon_was_running=true

cargo install --path "$repo_root" --locked --force --root "$install_root"

if [[ "$daemon_was_running" == true ]]; then
    pkill -x raylander
    for _ in {1..50}; do
        pgrep -x raylander >/dev/null 2>&1 || break
        sleep 0.1
    done
    nohup "$installed_binary" daemon >/dev/null 2>&1 &
fi
