# Glossary

Glow's domain terms. Use these words in code, issues, and docs.

## Brightness range

- **Total brightness**: the one 0–200% value Glow shows. 0–100% is system brightness; 100–200% is boost. _Avoid:_ level, value.
- **System brightness**: macOS's own 0–100% brightness of the main screen, read and set through `DisplayServices`.
- **Boost**: the 100–200% part of the range, shown as extra HDR brightness on capable screens.
- **Boost level**: how far into the boost the total is, as 0–1.
- **Crossover**: the 100% point where system brightness hands over to boost.
- **HDR wake**: switching a screen into extended dynamic range so it can be boosted.

## Displays

- **Synced dimming**: other displays following the system brightness below 100%, by hardware or software.
- **Synced display**: a screen other than the main one that follows synced dimming.
- **Software dimming**: lowering a display by scaling its colour table (or with a black overlay) when it has no hardware brightness control.

## Changes over time

- **Step**: one brightness key press or arrow press: 1/16 of the range.
- **Goal**: the total brightness Glow is heading to, set by the bar or keys. Matches `sys_goal` / `goal_total` in code. _Avoid:_ target.
- **Applied brightness**: the total brightness the screens show right now, moving toward the goal.
- **Fade**: the timed move from applied brightness to the goal.
- **Fast mode**: the short period when Glow updates every display frame.
- **Idle cadence**: Glow's slow background update rate (~14 Hz) when nothing is changing.
- **Read-back**: a system brightness value Glow reads from macOS, as opposed to one it wrote.
- **Settle window**: the time after Glow's own write during which read-backs are ignored.
- **Key-to-photon latency**: time from a key press to the first visible change on screen.

## Input and UI

- **Key observation**: seeing brightness key presses after macOS has handled them, without blocking them. _Avoid:_ intercept, take over.
- **Glow panel**: Glow's pop-up controls under the menu-bar icon. Pops up only for changes above 100%. _Avoid:_ HUD, bubble.
- **macOS brightness indicator**: the system's own on-screen brightness display. Glow never modifies it. _Avoid:_ OSD, bezel.
