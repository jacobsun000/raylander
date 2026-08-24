# Raylander

Raylander is a long-running server for the external dock web interface.

## Requirements

- Rust 1.90 or newer
- PipeWire/WirePlumber with `wpctl` and PulseAudio compatibility
- The system utilities used by dock controls, including `playerctl`, `nmcli`, and `hyprctl`

## Build and run

```sh
cargo build --release
cargo install --path . --locked
raylander daemon
```

`daemon` is the only command. Raylander uses the desktop application service to
keep a single running instance.

The dock is available at `http://HOST-IP:8030` by default. The `0.0.0.0` bind
address makes it reachable from the local network; do not expose this
unauthenticated service to the public internet.

## Configuration

Raylander reads `$XDG_CONFIG_HOME/raylander/config.toml`, falling back to
`~/.config/raylander/config.toml`. See [`examples/config.toml`](examples/config.toml).
Configuration is read when the daemon starts, so restart the process after changes.

```toml
[components.external-dock]
enabled = true
bind-address = "0.0.0.0"
port = 8030
weather-latitude = "30.2672"
weather-longitude = "-97.7431"
google-calendar-urls = []
```

The dock stylesheet is generated from `assets/web/tailwind.css` and committed.
After changing it or the dock template, run:

```sh
npm install
npm run build:css
```

For development hot reload, install `cargo-watch` and run `./scripts/dev.sh`.
