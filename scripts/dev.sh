#!/usr/bin/env bash
set -euo pipefail

if ! command -v cargo-watch >/dev/null 2>&1; then
  echo "cargo-watch is required. Install it with: cargo install cargo-watch" >&2
  exit 1
fi

pkill -x raylander >/dev/null 2>&1 || true
exec cargo watch --clear --exec 'run -- daemon'
