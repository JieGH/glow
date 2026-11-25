# 2. No private APIs to suppress system brightness handling

Status: accepted (#2)

## Context
Private symbolic-hotkey or bezel APIs might stop macOS acting on brightness keys without a permission.

## Decision
Glow does not use them. `DisplayServices` stays the only private framework.

## Consequences
- Glow never hides, replaces, or modifies the macOS brightness indicator. This is the owner's decision, not only a limit.
- Glow avoids a fragile dependency that could leave system keys broken after a crash or a macOS update.
