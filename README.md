# Raylander

Raylander is a long-running, Wayland-native desktop shell and home for small
utilities. It provides Atlas-styled per-monitor menu bars, a unified sidebar,
desktop widgets, and the original volume OSD for Hyprland. Components share typed
state while keeping system adapters and GTK views isolated.

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

Alternatively, use the installation helper. Atlas CSS assets are embedded into
the installed binary; if a Raylander daemon is already running, the helper
restarts it with the newly installed build. It does not start a stopped daemon.

```sh
./scripts/install.sh
```

Raylander is a single-instance GTK application. Later commands are forwarded to
the running process:

```sh
raylander volume up
raylander volume down
raylander volume mute
raylander volume show
raylander mic mute
raylander sidebar toggle notifications
raylander media play-pause
raylander input next
raylander packages refresh
raylander notifications dnd toggle
raylander preview -t 5s
raylander reload
raylander quit
```

While the daemon is running, the external dock demo is available at
`http://HOST-IP:8030`. It displays “Hello World” and a clock streamed from the
server over a WebSocket. The default `0.0.0.0` bind address makes it reachable
from phones on the same network; do not expose this unauthenticated HTTP service
to the public internet. Configure or disable it under `[components.external-dock]`.
Because Raylander is single-instance, running `cargo run -- daemon` while a daemon
is active contacts the existing process rather than replacing it. Apply config
changes with `cargo run -- reload`; after rebuilding code, restart with
`cargo run -- quit` followed by `cargo run -- daemon`.

For UI development, use `cargo run -- preview`: it runs the just-compiled code in
a separate local process and does not contact or stop an older daemon. The
duration accepts human-friendly values such as `500ms`, `5s`, or `2m` and
defaults to five seconds. Use `cargo run -- preview --output preview.png` to render
the GTK/CSS preview to a PNG without showing it on screen; image mode exits after
capture. `./run.sh` starts a ten-second interactive preview.

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

The embedded theme implements the Atlas Desktop UI System and includes light and
dark semantic token sets. Set `color-scheme` to `system`, `light`, or `dark`;
`system` follows GTK's current appearance when Raylander starts or reloads.
Atlas colors are centralized in `assets/atlas-light.css` and
`assets/atlas-dark.css`, while shared component recipes live in
`assets/default.css`. New components should use semantic `@atlas_*` colors and
Atlas recipe classes instead of embedding palette values.

The stable CSS hooks include `.raylander-window`, `.atlas-menu-bar`,
`.atlas-sidebar`, `.atlas-tile`, `.volume-osd`, `.volume-icon`, `.volume-percent`,
and `.volume-bar`. Run `raylander reload` after editing either file. An invalid
reload keeps the previously active configuration and theme. See
[`docs/desktop-shell.md`](docs/desktop-shell.md) for the architecture and
presentation contracts.

## Architecture

- `app` owns application lifetime, single-instance command forwarding, and the
  typed command dispatcher.
- `audio` isolates `wpctl` control/query behavior and the restartable `pactl`
  event observer.
- `volume_osd` owns only its layer-shell window and visual state.
- `shell` owns menu-bar, sidebar, and desktop-widget GTK surfaces.
- `shell_sources` supervises blocking current-machine status adapters away from GTK.
- `config` owns XDG discovery, defaults, validation, and component settings.

New components should follow the same boundary: add a typed command, isolate any
compositor/system protocol in an adapter, own the component's GTK objects, and
place its settings under `[components.<name>]`. Runtime-loaded plugins are not a
goal; components are compiled into the daemon so interfaces remain type-safe.
