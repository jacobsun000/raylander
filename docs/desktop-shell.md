# Desktop shell architecture

Raylander is a single GTK application with several layer-shell surfaces backed by one typed state
model. System integration stays outside GTK code, and every presentation uses the same state rather
than querying the machine independently.

```text
Hyprland / MPRIS / PipeWire / NetworkManager / Fcitx / pacman
                            │
                      source supervisor
                            │ RuntimeEvent
                            ▼
                         ShellState
             ┌──────────────┼──────────────┐
             ▼              ▼              ▼
         menu bars       sidebar      desktop tiles
```

The source supervisor runs blocking commands away from GTK, bounds refresh rates, and preserves the
last good state when a service is unavailable. `ShellUi` only owns windows and semantic CSS hooks.
User actions cross typed command boundaries and never execute shell strings.

## Surface contract

- The menu bar is a top-layer, auto-exclusive window on every selected monitor. Active workspace
  content is monitor-local; global statuses are mirrored.
- The unified sidebar is an overlay on the compositor-selected focused monitor. Menu-bar status
  items and the `raylander sidebar` command toggle it.
- Desktop widgets are bottom-layer windows above the wallpaper and below normal clients. Placement
  is a validated per-monitor grid from configuration.
- Notifications and OSDs are non-focus-stealing overlays. The notification state engine supports
  freedesktop IDs, replacement, actions, DND, bounded history, and atomic persistence; the D-Bus
  transport is kept separate.

## Presentation contract

`1x2` means one column by two rows. The embedded Atlas recipe uses a 144 logical-pixel cell and a
16px gap. Components declare only useful presentations:

| Component | Presentations | Current-machine source |
|---|---|---|
| Now Playing | bar, 1x1, 1x2, 2x1, 2x2, sidebar | `playerctl` / MPRIS |
| Time | bar, 1x1, 2x1 | local GLib clock |
| Calendar | bar date, 1x1, 2x2, sidebar | local Gregorian calendar |
| Notifications | bar, 1x1, 1x2, 2x1, banner, sidebar | freedesktop notifications |
| Volume and microphone | bar, 1x1, 1x2, 2x1, sidebar, OSD | `wpctl` / `pactl` |
| Network | bar, 1x1, 2x1, sidebar | `nmcli` |
| Packages | bar, 1x1, 1x2, 2x1, sidebar | `checkupdates`, `paru -Qua` |
| Input method | bar, 1x1, sidebar | `fcitx5-remote` |
| Workspaces and active app | bar | Hyprland JSON/socket |
| Tray | bar | StatusNotifierItem |
| System health | alert, 1x1, 1x2, 2x1, 2x2 | `/proc`, `/sys` |
| Display, storage, power, privacy | relevant compact/tile/sidebar views | DDC, GIO, UPower, PipeWire |

Absent devices are availability states, not errors. Bluetooth, battery, brightness, and future
weather views stay hidden when their source is unavailable.

## Atlas boundary

Rust assigns classes such as `.atlas-menu-bar`, `.atlas-sidebar`, `.atlas-tile`, and `.size-2x1`.
All colors, fonts, icon sizing, spacing, widget geometry, radii, borders, elevation, opacity, and
motion live in the embedded light/dark token sheets and component recipes. User CSS loads after the
embedded provider and may replace any stable hook without recompiling Raylander.

## Safety and rollout

Network actions are limited to known profiles, package integration never installs updates, and
session-ending actions require confirmation. Raylander does not kill Waybar or Dunst or rewrite
Hyprland configuration. Disable those services only after their corresponding Raylander surfaces
have passed live Wayland validation.
