# Raylander

Raylander is a long-running, Wayland-native home for small desktop utilities. The
first component is a volume OSD for Hyprland; future components can add their own
system adapter, state, commands, and layer-shell window without sharing UI logic.

## Requirements

- Rust 1.90 or newer
- GTK 4.12 or newer
- gtk4-layer-shell
- PipeWire/WirePlumber with `wpctl` and the PulseAudio-compatible `pactl`

On Arch Linux, the runtime/build packages are `gtk4`, `gtk4-layer-shell`,
`pipewire`, `wireplumber`, and `libpulse`.

## Build and run

```sh
cargo build --release
cargo install --path . --locked
raylander daemon
```

Raylander is a single-instance GTK application. Later commands are forwarded to
the running process:

```sh
raylander volume up
raylander volume down
raylander volume mute
raylander volume show
raylander reload
raylander quit
```

Recommended Hyprland configuration:

```ini
exec-once = raylander daemon
binde = , XF86AudioRaiseVolume, exec, raylander volume up
binde = , XF86AudioLowerVolume, exec, raylander volume down
bind  = , XF86AudioMute,        exec, raylander volume mute
```

## Configuration and theming

Raylander reads `$XDG_CONFIG_HOME/raylander/config.toml`, falling back to
`~/.config/raylander/config.toml`. Missing configuration uses embedded defaults.
Copy [`examples/config.toml`](examples/config.toml) and
[`examples/theme.css`](examples/theme.css) to that directory to customize it.
Relative theme paths are resolved from the Raylander configuration directory.

The stable CSS hooks are `.raylander-window`, `.volume-osd`, `.volume-icon`,
`.volume-percent`, and `.volume-bar`. Run `raylander reload` after editing either
file. An invalid reload keeps the previously active configuration and theme.

## Architecture

- `app` owns application lifetime, single-instance command forwarding, and the
  typed command dispatcher.
- `audio` isolates `wpctl` control/query behavior and the restartable `pactl`
  event observer.
- `volume_osd` owns only its layer-shell window and visual state.
- `config` owns XDG discovery, defaults, validation, and component settings.

New components should follow the same boundary: add a typed command, isolate any
compositor/system protocol in an adapter, own the component's GTK objects, and
place its settings under `[components.<name>]`. Runtime-loaded plugins are not a
goal; components are compiled into the daemon so interfaces remain type-safe.
