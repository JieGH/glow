# 1. Glow requests no privacy permissions

Status: accepted (#2)

## Context
Taking brightness keys before macOS sees them needs Accessibility (active event tap). A listen-only tap or IOHID needs Input Monitoring. Both make the user approve Glow by hand in System Settings.

## Decision
Glow uses no API that triggers a privacy permission prompt. It uses key observation through `NSEvent` SystemDefined monitors only.

## Consequences
- macOS always dims first and shows the macOS brightness indicator; Glow corrects afterwards.
- Smoothness near 100% depends on a fast observation path (#3).
- Glow's panel pops up only for changes above 100%, which the macOS indicator cannot show. Below 100% the macOS indicator is enough.
- Any future feature that needs a permission requires a new ADR.
