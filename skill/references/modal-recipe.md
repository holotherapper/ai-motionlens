# Recipe: modal / drawer / popover open-close

## Pattern

A trigger (click on a button) causes a panel to fade and/or slide into view, then back out. Critical things to check:

1. Did the panel actually appear?
2. Is the fade-in smooth (no flash, no skip)?
3. Does the duration match design intent?
4. Are there layout anomalies at any moment (text truncated, button overflow, stacking conflict)?

## Steps

```yaml
1. session.launch(url)
2. session.capabilities  # always
3. episode.start
4. episode.set_intent:
     description: "Modal fades in over 300ms with subtle slide-up"
     expected_duration_ms: 300
     expected_kinds: [fade, translate]
     expected_targets: ["#confirm-modal"]
     forbidden_kinds: [disappearance]   # nothing should suddenly vanish

5. trigger.click(selector: "#open-modal")
6. frame.capture_series:
     target_times_ms: [0, 60, 120, 200, 320, 500]
     layout: { selectors: ["#confirm-modal", "#modal-backdrop"] }

7. motion.contact_sheet(frame_ids: <all from step 6>)
   → Read the artifact_local_path. One image = the whole open animation.

8. motion.assess(frame_ids: <all>):
   → Check smoothness, jank_events, intent_match.
   → If intent_match.passes == false, the animation does not meet the Plan.
```

## What "good" looks like

- `motion.assess.smoothness ≥ 0.7`
- `intent_match.passes == true`
- No `text-ellipsis-truncated` / `content-clipped` / `off-screen` in any frame's `layout_snapshot.anomalies` (unless the off-screen state is *during* the slide, then verify it resolves by the last frame)
- The contact sheet shows monotonic opacity rise and translate

## Common failures and where they show up

| Symptom in motion.assess | Likely cause |
|---|---|
| `mean_delta ≈ 0` | Animation not registered (check `timeline.sources` first, then `dom_query` for `animation-play-state`) |
| `smoothness < 0.6` + 1 large `jank_event` | A class is being added/removed late, causing a single-frame jump |
| `intent_match.expected_kinds_missing: [fade]` | You declared fade but only saw translate - verify `opacity` is animated, not just `transform` |
| `duration_match.verdict: too-long` | Likely `transition-duration` is set higher than intended, or the modal waits for a network call |

## Closing the modal

Symmetric: trigger the close (click outside / Esc) and run the same `capture_series` with new `target_times_ms` over the expected close duration. A modal that opens at 300ms but closes at 600ms is usually a bug.
