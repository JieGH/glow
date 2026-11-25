# 6. Glow's own value wins over read-backs during its changes

Status: accepted (#4)

## Context
While Glow changes brightness, macOS reports lagging or rounded read-backs, which pulled the value backwards.

## Decision
Ignore read-backs during a fade and the settle window after it. After that, treat a read-back as an outside change only if it differs from Glow's last write by more than 0.01.

## Consequences
- An outside change (Control Center) made during a Glow drag is lost until the drag ends.
- Outside changes under 0.01 are ignored.
