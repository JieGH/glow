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
- Glow opens at 120% (`LAUNCH_LEVEL`) with auto boost on. The switch is not saved between launches.
  With it on, pressing brightness-up at full brightness goes past 100%.
- One key press moves 1/16 of the range. Changes fade by elapsed time: each update covers
  `1 − exp(−dt / FADE_TAU)` of the remaining distance (`FADE_TAU` = 25 ms), so a step lands in
  about 80 ms at any update rate. The total moves at most 3.0 per second (`SPEED_CAP`).
  One eased total drives every screen: the system value, the synced displays, software dimming
  and the boost are all written from it, so they move together. Dragging the bar only sets the
  goal; the fade follows it. A key press wakes the timer at once. While anything fades, or
  for 600 ms after a key press, the timer runs once per display frame; otherwise it runs about
  14 times a second.
- Glow needs no privacy permission. It watches brightness key presses without taking them, so
  macOS always dims first and shows its own indicator. Glow follows and corrects:
  - Brightness-down while boosted: macOS dims the screen one step and Glow can't stop it (it
    ignores Glow's writes to the system brightness). Once the dimming has stood still for 60 ms
    (it restarts while macOS is still moving), Glow moves it into the boost: system back to full, boost down by the same amount.
  - Brightness-up at full brightness adds one step of boost.
  - With no screen that has its own brightness (lid closed, external displays only) and Sync on,
    macOS ignores the keys, so Glow moves the total one step across 0–200% itself.
  - Lowering the brightness another way (Control Center) while boosted takes the boost down by the
    same amount, after the same 60 ms wait.
- After Glow sets the system brightness itself, it trusts its own value for 0.4 s so the slider
  doesn't jump.

## Synced dimming below 100%

**Sync all displays** is on by default and saved between launches. Below 100%, every connected
display dims together:

- Displays with their own brightness control follow the main screen through `DisplayServices`.
- Other external displays are dimmed over DDC (their own backlight control) if
  [`m1ddc`](https://github.com/waydabber/m1ddc) is installed and can read their luminance. See
  below.
- The rest are dimmed in software by scaling their saved colour table by
  `0.05 + 0.95 × level`. The 5% floor keeps a screen from going fully black.
- If a display's colour table can't be read, Glow uses a click-through black overlay window
  instead.
- While a display is software-dimmed, Glow checks once a second whether macOS reset its colour
  table (for example after waking from sleep) and reapplies the dimming.
- If no connected screen has its own brightness control (for example a closed laptop with external
  monitors), Glow keeps its own 0–100% value so the slider and keys still dim every monitor.

### DDC dimming

- Glow looks for `m1ddc` in `/opt/homebrew/bin` and `/usr/local/bin`. Without it, all of these
  displays use software dimming.
- When a dim session starts, Glow reads the monitor's luminance as the baseline. It then writes
  `round(baseline × level)`, so it never goes above your own setting. It does not also dim in
  software. At 0% the luminance is 0; how dark that looks depends on the monitor.
- If your monitor still looks brighter than the Mac when dimmed, make it dim faster with
  `defaults write io.github.jiegh.glow ddcCurve -float 1.5`, then reopen Glow. The monitor gets
  `round(baseline × level^ddcCurve)`. Default 1.0, range 0.25–4. It applies to all DDC monitors.
- At 100%, with Sync off, or on quit, Glow writes the baseline back and forgets it. The next
  session reads it again. Quit waits up to 5 s for this. If Glow never dimmed, it never writes.
- A background thread runs `m1ddc` and sends only the latest value, skipping repeats. Each command
  is stopped after 2 s.
- If the probe fails, the display uses software dimming until it is reconnected. If a later read or
  write fails, it uses software dimming until dimming next starts or ends. Glow keeps the baseline
  until a restore succeeds. If restoring also fails, the monitor can stay dimmed, with software
  dimming on top.
- A reconnected monitor keeps its baseline while Glow runs.
- **A force-quit or crash leaves the monitor at the dimmed value.** The same can happen if you
  unplug a dimmed monitor and then quit Glow. Reset it with the monitor's own buttons.

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
| Speed cap | 3.0 total/s | `SPEED_CAP` in `src/boost.rs` |
| Read-back noise | 0.01 | `READBACK_NOISE` in `src/main.rs` |
| Fade time constant | 25 ms | `FADE_TAU` in `src/boost.rs` |
| Software-dim floor | 0.05 | `DIM_FLOOR` |
