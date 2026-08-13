#!/usr/bin/env bash
set -euo pipefail

if ! command -v cargo-watch >/dev/null 2>&1; then
  echo "cargo-watch is required. Install it with: cargo install cargo-watch" >&2
  exit 1
fi

# Raylander is single-instance. Stop an existing daemon so cargo-watch always
# launches the binary built from this working tree.
cargo run --quiet -- quit >/dev/null 2>&1 || true

# cargo-watch watches the project by default (excluding target/ and .git/) and
# restarts a running command when a relevant file changes.
exec cargo watch --clear --exec 'run -- daemon'
