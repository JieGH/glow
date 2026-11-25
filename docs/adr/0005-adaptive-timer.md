# 5. One adaptive timer instead of a display link

Status: accepted (#3)

## Context
Fades need display-rate updates, but idle must stay cheap. Glow supports macOS 13. `CVDisplayLink` is deprecated; `NSScreen.displayLink` needs macOS 14.

## Decision
Reuse the existing background timer. A key press moves its next fire date to now. In fast mode (a fade in progress, the pointer down on the bar, or 600 ms after a key press) each tick schedules the next one a frame ahead. Otherwise it keeps the idle cadence.

## Consequences
- No new thread, no deprecated API.
- Updates are not locked to the screen refresh and can jitter by about a frame; the time-based fade (ADR 4) hides this.
- Minimum macOS stays at 13 (owner decision).
- Revisit if the minimum macOS rises to 14.
