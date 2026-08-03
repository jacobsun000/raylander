# Repository Guidelines

## Project Structure & Module Organization

Raylander is a Rust 2024 GTK application. `src/main.rs` is the executable entry point, while `src/lib.rs` exposes the application runner. Keep responsibilities separated across the existing modules: `app.rs` manages lifecycle and command dispatch, `audio.rs` integrates with `wpctl` and `pactl`, `config.rs` handles configuration and validation, `cli.rs` defines commands, and `volume_osd.rs` owns the GTK layer-shell UI. Default styling lives in `assets/default.css`; user-facing configuration and theme samples live in `examples/`.

New desktop components should follow the same pattern: define typed commands, isolate system integration in an adapter, keep GTK state within the component, and place settings under `[components.<name>]`.

## Build, Test, and Development Commands

- `cargo build` compiles a debug build for local development.
- `cargo build --release` produces the optimized binary.
- `cargo run -- daemon` starts Raylander from the working tree.
- `cargo test` runs all unit tests.
- `cargo fmt --all -- --check` verifies standard Rust formatting.
- `cargo clippy --all-targets --all-features -- -D warnings` treats lint warnings as errors.
- `cargo install --path . --locked` installs the exact dependency versions in `Cargo.lock`.
- `cargo run -- demo -t 5s` displays every component preview for five seconds and then exits.
- `cargo run -- demo --output /tmp/raylander-demo.png` renders the preview to a PNG without displaying it.

Building requires Rust 1.90+, GTK 4.12+, and gtk4-layer-shell. Runtime audio behavior also requires `wpctl` and `pactl`.

## Coding Style & Naming Conventions

Use `rustfmt` defaults (four-space indentation) and keep code Clippy-clean. Follow Rust conventions: `snake_case` for modules, functions, and variables; `PascalCase` for types and traits; and `SCREAMING_SNAKE_CASE` for constants. Prefer typed command/state boundaries over stringly typed interfaces. Add contextual errors with `anyhow` where failures cross system or process boundaries.

## Testing Guidelines

Place focused unit tests in a `#[cfg(test)] mod tests` beside the code under test, following existing examples in `cli.rs`, `audio.rs`, and `config.rs`. Name tests after observable behavior, such as `parses_volume_command`. Run `cargo test` before submitting changes. There is no fixed coverage threshold; prioritize parsing, validation, state transitions, and failure cases.

For visual changes, render `/tmp/raylander-demo.png` with `cargo run -- demo --output /tmp/raylander-demo.png`, inspect the resulting image, and compare spacing, color roles, typography, borders, elevation, and component state against the Atlas Desktop UI System. Also run `cargo run -- demo -t 5s` in a Wayland session to verify placement, scaling, transitions, and compositor behavior that a still image cannot cover. Repeat both checks in `light` and `dark` color schemes when shared tokens or theme CSS changes.

## Commit & Pull Request Guidelines

Use Conventional Commits, for example `feat: add brightness OSD` or `fix(audio): handle missing default sink`. Keep commits focused. Pull requests should explain the user-visible change, note configuration or dependency impacts, link relevant issues, and include screenshots or recordings for UI changes. Report the commands used to validate the change.
