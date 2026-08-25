# Raylander

Raylander is a long-running server for the external dock web interface. Linux uses a console daemon workflow; the x86_64 Windows package also includes a click-to-run background application with a notification-area menu and Codex lifecycle-hook integration.

## Requirements

- Rust 1.90 or newer when building from source
- Linux: PipeWire/WirePlumber with `wpctl`, PulseAudio compatibility, and the system utilities used by enabled dock controls (for example `playerctl`, `nmcli`, and `hyprctl`)
- Windows: a Codex client that supports user lifecycle hooks for agent runtime status; other enabled components use native Windows APIs where available

## Linux build and run

```sh
cargo build --release
cargo install --path . --locked --bin raylander
raylander daemon
```

The console `daemon` behavior remains the normal Linux lifecycle. `display-power` sends the existing local display-power invalidation to a running daemon.

## Windows

Download and extract the Windows ZIP artifact, keeping both executables together, then double-click `raylander-tray.exe`. It starts the dock without a console window, attempts to install the Codex lifecycle hooks when the agents component is enabled, and remains available in the notification area. A hook-installation error does not stop the dock; use the menu or CLI to retry and view the error. The menu can open the dock or configuration folder, reinstall hooks, and exit gracefully.

`raylander.exe` remains a console CLI for scripting and diagnostics:

```powershell
raylander.exe daemon
raylander.exe hooks status
raylander.exe hooks install
raylander.exe hooks remove
```

The internal `raylander.exe codex-hook` command receives hook JSON from Codex on standard input. Windows `daemon` startup and the tray both attempt to install or repair the hooks when `[components.agents]` is enabled. Codex may ask you to review and trust newly installed or changed user hooks before they run.

See [Windows packaging and usage](docs/windows.md) for ZIP contents, configuration, troubleshooting commands, and local packaging instructions.

## Dock address and security

The dock is available at `http://HOST-IP:8030` by default. The `0.0.0.0` bind address makes it reachable from the local network; do not expose this unauthenticated service to the public internet.

## Configuration

Raylander reads:

- Linux: `$XDG_CONFIG_HOME/raylander/config.toml`, falling back to `~/.config/raylander/config.toml`
- Windows: `%APPDATA%\Raylander\config.toml`

See [`examples/config.toml`](examples/config.toml) for all component settings. Configuration is read when the daemon starts, so restart the process after changes.

```toml
[components.external-dock]
enabled = true
bind-address = "0.0.0.0"
port = 8030

[components.dock-layout]
columns = 2
order = []

[components.weather]
enabled = true
latitude = "30.2672"
longitude = "-97.7431"
```

## Development

The dock stylesheet is generated from `assets/web/tailwind.css` and committed. After changing it or the dock template, run:

```sh
npm install
npm run build:css
```

For development hot reload, install `cargo-watch` and run `./scripts/dev.sh`.

Before submitting changes, run:

```sh
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
```
