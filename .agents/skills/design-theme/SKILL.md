---
name: Atlas Desktop UI System
description: For building desktop utilities (menu bars, widgets, launchers, panels, settings) in the Atlas visual language.
---

For building desktop utilities (menu bars, widgets, launchers, panels, settings) in the Atlas visual language. Target: GTK4 (gtk-rs). All values are theme tokens — implement both light and dark.

## Principles

1. **Hairline, not glass.** Surfaces are defined by 1px borders + subtle shadow, never heavy blur/refraction. Translucency only on overlays that float above the desktop (menus, Control Center, launcher).
2. **One accent.** Neutrals do the work. The accent marks the current action, selection, and on-state — nothing else. Status colors appear only for status.
3. **Motion is functional.** 150ms ease-out micro-feedback, 250ms panels. Overlays scale from 0.97 and rise 8px. Never bounce, never slide across the screen.
4. **Read-first widgets.** One display-size value, captions for the rest, at most one control.
5. **8px grid.** All padding/gaps from {4, 8, 12, 16, 24, 32, 48}.

## Tokens

### Color roles

| Role | Light | Dark | GTK name suggestion |
|---|---|---|---|
| bg (canvas) | `#F3F4F6` | `#0B0C0F` | `@bg` |
| bg2 (recessed) | `#EAECEF` | `#111319` | `@bg2` |
| surface | `#FFFFFF` | `#16181E` | `@sf` |
| surface2 (fields, sidebars) | `#F6F7F9` | `#1D2029` | `@sf2` |
| border | `rgba(16,20,28,.09)` | `rgba(255,255,255,.09)` | `@bd` |
| border-strong (hover) | `rgba(16,20,28,.16)` | `rgba(255,255,255,.18)` | `@bd2` |
| text | `#15181E` | `#F1F2F5` | `@tx` |
| text2 (secondary) | `#5A6170` | `#A6ACB9` | `@tx2` |
| text3 (tertiary/disabled) | `#9AA1AE` | `#666D7B` | `@tx3` |
| accent (default) | `#0A7CFF` | same | `@ac` |
| success / warning / danger | `#22A857` / `#E8A13A` / `#E5484D` | same | — |

Accent is user-tweakable; alternates: `#7C5CFF`, `#00A87E`, `#FF6B4A`. Derived washes use alpha mixes of accent/text: selection bg = accent @14%, hover wash = text @7%, focus ring = accent @20%, status badge bg = status color @13–18%.

### Radius (3 tiers)

- `r1 = 7px` — controls (buttons, fields, menu rows)
- `r2 = 11px` — menus, cards, grouped rows
- `r3 = 18px` — windows, widgets, overlay panels

Optional density variants: crisp (4/7/12), soft (11/16/24).

### Elevation (3 tiers, pair with radius)

- `e1` light `0 1px 2px rgba(15,18,26,.06)` / dark `0 1px 2px rgba(0,0,0,.4)` — controls
- `e2` `0 6px 20px .10` / `0 8px 24px .45` — menus, popovers, widgets
- `e3` `0 24px 60px .24` / `0 28px 70px .6` — windows, launcher, Control Center

### Type

- Sans: system UI font. Display: Space Grotesk (semibold, tight tracking −0.02em). Mono: IBM Plex Mono (shortcuts, values, meta).
- Scale: 32 display · 22 title · 15 headline (semibold) · 13 body (1.5 lh) · 11 caption · 12 mono.
- 13px is the default UI size. Captions/meta may be letterspaced +0.1em uppercase mono.

### Translucent overlays

Overlay surfaces = surface color at 62–92% opacity + background blur ~24px, saturation ~1.4, plus the normal 1px border. Menu bar 68%, dock 62%, menus 92%, widgets 80%, Control Center 88%. If "reduce transparency": solid surface.

## Component anatomy

**Control metrics:** default height 28px (small 22, large 34), padding 0 14px, font 13/500, radius r1.

- **Button.** Primary: accent fill, white text — max one per view. Secondary: surface + border. Ghost: transparent, text2 → 7% wash on hover. Destructive: danger @12% bg, danger text. Hover brightens/strengthens border; press scales 0.97; disabled: surface2 + text3.
- **Field.** 30px, surface2 bg, border; on focus: surface bg, accent border + 3px accent@20% ring. Search: leading ⌕ glyph, optional trailing kbd chip.
- **Kbd chip.** Mono 10px, 1px border (2px bottom), 5px radius.
- **Segmented control.** bg2 track, 2px inset; active segment: surface + e1.
- **Tabs.** Text-only, 2px accent underline on active, text3 → text on hover.
- **Switch.** 38×22 pill; off = border-strong fill, on = accent; 18px white knob, 200ms.
- **Checkbox/radio/slider.** Native, accent-tinted, 15px.
- **Menu.** Panel r2·e2 (context menus/dropdowns get e3 if floating over desktop), 4px inner padding. Row: r1, 5–6px×9px padding, 13px; shortcut in mono 11 @60%. Hover paints full accent + white text. Disabled rows stay hoverless in text3. Divider: 1px border color, inset 9px.
- **Popover.** r2·e2, 45°-rotated square arrow, headline + body + right-aligned actions.
- **Tooltip.** Inverted (text-color bg, bg-color text), 11px, 6px radius, appears above.
- **Badge.** Pill, 11px semibold; neutral = bg2/text2, others = status @~15% bg + status text. Count badge: danger fill, white, 18px min.
- **List row.** r1, 8×10px padding, 30px icon tile (8px radius, gradient), name 13/500 + meta 11 text3, chevron. Selected = accent @14%; hover = 7% wash.
- **Table.** Bordered r1 container; header row surface2, 11px semibold text3 letterspaced; rows 12px, hairline dividers, hover surface2.
- **Progress.** 6px pill track (bg2) + accent fill. Indeterminate: 18px ring, 2px, 0.8s linear spin. Activity: 3 bars, 3px, staggered.
- **Window.** r3·e3, traffic lights (#FF5F57 #FEBC2E #28C840, 12px). Settings pattern: 220px surface2 sidebar (search field + nav rows w/ 22px colored icon tiles, selected = accent @14%) + content pane of grouped-row cards (r2 border container; rows 13px×16px padding, hairline dividers; label + optional 11px sub, control right-aligned).
- **Widget.** r3, translucent 80%, 18–20px padding, e2. One display-size value; mono captions.
- **Menu bar.** 32px, translucent 68%, hairline bottom border. App name semibold; menu titles 13px, 3×10px padding, open = 7% wash + 5px radius. Right cluster: status glyphs 14px gap, mono meta, clock 12/500.
- **Dock.** Floating pill (r3+4), translucent 62%, 46px icon tiles (12px radius), 3px running-dot below, hover scale 1.16 + lift 5px.
- **Notification.** 330px, r3, translucent 85%, e2; 34px app tile, title 12/600 + time, body 12 text2, ✕ on hover.

## Recipes (compose, don't invent)

- **Menu** = surface(r2·e2) + rows(r1) + kbd + hairline dividers
- **Widget** = translucent surface(r3) + display type + live value
- **Control tile** = 30px circle (accent when on, 7% wash when off) + headline + caption
- **Launcher** = full blur scrim + centered search field + 6-col icon grid (64px tiles, r16)
- **Settings pane** = sidebar nav + grouped-row cards
- **Spotlight** = dark 25% scrim + 560px panel(r3·e3): 19px borderless input row + section caption + list rows w/ full-accent hover

## GTK4 / gtk-rs notes

- Ship tokens as a CSS provider (`gtk::CssProvider`); define light/dark via `@define-color` pairs and switch with `AdwStyleManager` color-scheme, or load alternate CSS. Name colors as in the token table.
- Map: r1/r2/r3 → `border-radius` in a `.atlas-control/.atlas-card/.atlas-window` class set; elevation → `box-shadow`; hover wash → `alpha(@tx, .07)`; selection → `alpha(@ac, .14)`.
- Translucency: GTK windows need a transparent visual + compositor blur is not portable — acceptable fallback: solid `@sf` surfaces (the "reduce transparency" mode is the spec'd fallback, not a degradation).
- Widgets: `GtkButton` (+`.pill`, `.flat` for ghost), `GtkSwitch` (restyle to 38×22), `GtkPopoverMenu` for menus (override row hover to accent), `GtkListBox` for list rows/grouped settings rows, `AdwPreferencesWindow`-like layout for Settings.
- Press feedback: `transform: scale(.97)` works in GTK4 CSS on `:active`.
- Motion: `transition: 150ms ease-out` on controls, 250ms on panels.

## Do / Don't

- Do keep one filled (primary/accent) control per view.
- Do use mono for every numeric/meta/shortcut string.
- Don't use glassmorphism, gradients on surfaces (gradients are for app icons only), or more than 2 background colors per screen.
- Don't invent new component shapes — compose the recipes above.
