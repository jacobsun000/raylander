#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/.." && pwd)
install_root=${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}
installed_binary="$install_root/bin/raylander"

required_assets=(
    assets/atlas-light.css
    assets/atlas-dark.css
    assets/default.css
)

for asset in "${required_assets[@]}"; do
    if [[ ! -f "$repo_root/$asset" ]]; then
        echo "raylander: required embedded asset is missing: $asset" >&2
        exit 1
    fi
done

daemon_was_running=false
if pgrep -x raylander >/dev/null 2>&1; then
    daemon_was_running=true
fi

echo "Building and installing Raylander to $installed_binary"
cargo install --path "$repo_root" --locked --force --root "$install_root"

if [[ "$daemon_was_running" != true ]]; then
    echo "Raylander installed; daemon was not running, so it was not started."
    exit 0
fi

echo "Restarting the running Raylander daemon"
"$installed_binary" quit

for _ in {1..50}; do
    if ! pgrep -x raylander >/dev/null 2>&1; then
        break
    fi
    sleep 0.1
done

if pgrep -x raylander >/dev/null 2>&1; then
    echo "raylander: daemon did not stop; refusing to start a second instance" >&2
    exit 1
fi

nohup "$installed_binary" daemon >/dev/null 2>&1 &

for _ in {1..50}; do
    if pgrep -x raylander >/dev/null 2>&1; then
        echo "Raylander daemon restarted."
        exit 0
    fi
    sleep 0.1
done

echo "raylander: daemon did not start successfully" >&2
exit 1
