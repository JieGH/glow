# 4. Fades run by elapsed time

Status: accepted (#3, #4)

## Context
`SMOOTH` (62% per tick) tied fade speed to the ~14 Hz tick, so a faster tick also meant a faster fade, and each tick could jump far.

## Decision
Each update moves `1 − exp(−dt / FADE_TAU)` of the remaining distance. The first update after idle uses one frame for `dt`. A speed cap (proposed 3.0 total/s) limits single-frame jumps.

`FADE_TAU` is 40 ms (a step lands in ~120 ms), shared by #3 and #4. Tune by eye only if it looks wrong. Speed cap: 3.0 total/s.

## Consequences
- The update rate can change freely.
- `SMOOTH` and its tests are replaced; `docs/how-it-works.md` must be updated.
- If frame-rate `DisplayServices` writes pulse the built-in screen, cap system-brightness writes at ~30 Hz; gamma and software dimming stay at display rate.
- Fast drags trail the pointer briefly.
