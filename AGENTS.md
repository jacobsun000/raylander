# Repository Guidelines

## Project Structure & Module Organization

Raylander is a Rust 2024 server for the external dock web interface. It does not provide GTK, layer-shell, or desktop-widget UI.

`src/main.rs` is the executable entry point, `src/lib.rs` exposes the runner, `src/app.rs` owns daemon lifecycle, `src/cli.rs` defines the `daemon` command, and `src/config.rs` handles typed configuration and validation. `src/web_server.rs` is a small facade over the dock server.

Each component lives directly under `src/components/<id>/` and owns its HTML, overlays, JavaScript, routes, state, observers, rendering, configuration, and backend adapters. Keep adapters under their owning component rather than adding component-specific modules at `src/*.rs` or nesting components below `src/components/dock/`. The `src/components/dock/` directory contains only shared dock composition, Axum/Tokio lifecycle, shell integration, invalidation transport, and framework assets.

The central layout in `src/components/dock/mod.rs` owns component ordering and placement. Components expose only their grid size and must not encode rows, columns, slots, or ordering.

Web styling and framework assets live under `assets/web/`; the page shell lives in `templates/external-dock.html`. User-facing configuration samples live in `examples/`. Android external-dock integration lives under `android/`.

## Build, Test, and Development Commands

- `cargo build` compiles a debug build.
- `cargo build --release` produces the release server.
- `cargo run -- daemon` starts the external dock server from the working tree.
- `cargo test` runs all unit tests.
- `cargo fmt --all -- --check` verifies Rust formatting.
- `cargo clippy --all-targets --all-features -- -D warnings` treats warnings as errors.
- `cargo install --path . --locked` installs the locked dependency set.
- `npm install` installs web asset tooling.
- `npm run build:css` regenerates the committed dock stylesheet after CSS/template changes.
- `./scripts/dev.sh` runs the development hot-reload workflow when `cargo-watch` is installed.

Building requires Rust 1.90+. Runtime features use system services and commands such as `wpctl`, `pactl`, `playerctl`, `nmcli`, `tailscale`, `hyprctl`, `fcitx5-remote`, `ddcutil`, and BlueZ where their corresponding components are enabled.

## Coding Style & Component Boundaries

Use `rustfmt` defaults and keep the project Clippy-clean. Follow Rust naming conventions. Prefer typed state, commands, configuration, invalidation keys, and backend results over stringly typed interfaces. Add contextual `anyhow` errors at process, filesystem, network, D-Bus, and parsing boundaries.

Adding or changing a component should not require component-specific logic in `server.rs`, the page shell, or shared JavaScript. Use `Component::routes()` for namespaced routes and component-owned assets. Background observers must use the shared shutdown token, return tracked lifecycle handles, and terminate cleanly when the server stops.

Do not reintroduce GTK/GIO application lifecycle, layer-shell windows, desktop widgets, OSDs, sidebars, preview commands, or desktop-only component infrastructure.

## Configuration Guidelines

Place component settings under `[components.<id>]`. Keep bind address and port under `[components.external-dock]`, and layout columns/order under `[components.dock-layout]`. Validate unknown, duplicate, and disabled component references. Update `examples/config.toml` whenever the public configuration shape changes.

## Testing Guidelines

Place focused unit tests beside the code under test. Prioritize parsing, validation, renderer output, route behavior, state transitions, observer invalidation, backend failures, component registration, layout validation, and shutdown behavior. Backend logic should be testable without live desktop services where practical.

For web UI changes, inspect the external dock in both supported color schemes and verify the Android/browser runtime for spacing, typography, color roles, scaling, dialogs, WebSocket updates, and touch behavior. Regenerate CSS when required.

Before submitting changes, run:

```sh
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
```

## Commit & Pull Request Guidelines

Use Conventional Commits, for example `feat(weather): add configurable locations` or `fix(media): handle missing player`. Keep commits focused. Pull requests should explain user-visible behavior, configuration and dependency impacts, linked issues, and validation commands. Include screenshots or recordings for visual changes.
