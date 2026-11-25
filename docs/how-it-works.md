# How Glow works

Glow gives you one brightness control from 0% to 200%:

- **0–100%** is macOS's own brightness, the same control the brightness keys and Control Center use.
- **100–200%** keeps the system at full brightness and adds a boost on HDR-capable screens.

## Boost above 100%

1. **Wake HDR.** On each HDR-capable screen, Glow opens a 5 pt borderless window filled with a
   colour brighter than white (1.6 in extended linear Display P3, RGBA16Float, EDR on). This makes
   macOS switch that screen into its extended dynamic range (EDR) mode.
2. **Wait for headroom.** Glow polls the screen's EDR headroom. Once it rises above 1.05, the screen
   is ready. If it doesn't within 10 s, Glow gives up on that screen and leaves it unchanged.
3. **Check the colour table.** When the boost turns on, Glow saves the screen's colour table
   (`CGGetDisplayTransferByTable`). If another app changes it while HDR is waking up (checked at
   the quarter sample, tolerance 0.03), Glow leaves that screen alone.
4. **Boost.** Glow multiplies each R, G and B sample of the saved table by
   `1 + (headroom − 1) × level` and writes it back (`CGSetDisplayTransferByTable`). The factor
   follows the headroom as macOS ramps it up.
5. **Restore.** Turning the boost off writes the saved table back. Once every screen is back to
   normal, or on quit, Glow calls `CGDisplayRestoreColorSyncSettings`.

HDR only stays awake while there is a boost to show. It sleeps again 1 s after returning to 100%.

## Brightness keys and the system brightness

- Glow reads and sets the system brightness through `DisplayServices`, a private macOS framework
  loaded at run time. This is why Glow isn't on the Mac App Store.
- Glow starts with the boost armed at plain 100%, so nothing changes at launch. Pressing
  brightness-up at full brightness goes past 100%.
- One key press moves 1/16 of the range. Changes fade by elapsed time: each update covers
  `1 − exp(−dt / FADE_TAU)` of the remaining distance (`FADE_TAU` = 40 ms), so a step lands in
  about 120 ms at any update rate. A key press wakes the timer at once. While anything fades, or
  for 600 ms after a key press, the timer runs once per display frame; otherwise it runs about
  14 times a second.
- Glow needs no privacy permission. It watches brightness key presses without taking them, so
  macOS always dims first and shows its own indicator. Glow follows and corrects:
  - Brightness-down while boosted takes one step off the boost and holds the system at full for
    600 ms (re-asserted every frame; a down press with no boost left ends the hold), so the system's own dimming can't drain the boost.
  - Brightness-up at full brightness adds one step of boost.
  - With no screen that has its own brightness (lid closed, external displays only) and Sync on,
    macOS ignores the keys, so Glow moves the total one step across 0–200% itself.
  - Lowering the brightness another way (Control Center) while boosted takes the boost down by the
    same amount, after 150 ms in case a key press is on its way.
- After Glow sets the system brightness itself, it trusts its own value for 0.4 s so the slider
  doesn't jump.

## Synced dimming below 100%

**Sync all displays** is on by default and saved between launches. Below 100%, every connected
display dims together:

- Displays with their own brightness control follow the main screen through `DisplayServices`.
- Other external displays are dimmed in software by scaling their saved colour table by
  `0.05 + 0.95 × level`. The 5% floor keeps a screen from going fully black.
- If a display's colour table can't be read, Glow uses a click-through black overlay window
  instead.
- While a display is software-dimmed, Glow checks once a second whether macOS reset its colour
  table (for example after waking from sleep) and reapplies the dimming.
- If no connected screen has its own brightness control (for example a closed laptop with external
  monitors), Glow keeps its own 0–100% value so the slider and keys still dim every monitor.

## The menu-bar icon

Glow has no Dock icon, only a sun in the menu bar.

- **Left click** opens or closes the controls in a panel under the icon. Clicking elsewhere closes
  it.
- **Right click** (or Control-click) opens a menu: Show Controls, Turn Boost On/Off, Turn Display
  Sync On/Off, and Quit Glow (⌘Q).
- **Show on change** (on by default) pops the panel up without taking focus when Glow handles a
  brightness key or the brightness moves above 100%. It stays while the pointer is over it, then
  fades after 1.5 s (0.3 s fade).
- **Quit** restores every screen before exiting.

## Tunable constants

| Constant | Value | Where |
|---|---|---|
| Key steps across the range | 16 | `STEPS` in `src/main.rs` |
| HDR trigger colour / size | 1.6 / 5 pt | `TRIGGER_VALUE`, `TRIGGER_SIZE` in `src/boost.rs` |
| Minimum EDR headroom | 1.05 | `EDR_MIN_HEADROOM` |
| HDR wake timeout | 10 s | `GATE_TIMEOUT` |
| Colour-table drift tolerance | 0.03 | `GAMMA_DRIFT` |
| Fade time constant | 40 ms | `FADE_TAU` in `src/boost.rs` |
| Software-dim floor | 0.05 | `DIM_FLOOR` |
