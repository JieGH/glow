# 3. One eased value drives every brightness output

Status: accepted (#4)

## Context
The bar, the keys, and the booster each eased their own outputs. Screens lagged each other and fast drags flickered.

## Decision
Inputs set only the goal. One stepper fades one applied total and writes every output (system brightness, software dimming, boost) from it. No output eases a second time.

## Consequences
- The second ease in `Booster::tick` goes away.
- #3 and #4 share this pipeline.
- Any future input (shortcuts, scripts) must go through the goal.
