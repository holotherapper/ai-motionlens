# Recipe: loop / spinner / infinite scroller

The trap: a loop animation can look identical at t=0 and t=T. If you only sample those two frames, the animation looks broken (no motion) even when it is fine, or you miss a real freeze.

## Steps

```yaml
1. session.launch(url)
2. timeline.sources
   # Find the spinner's animation in `css_animations[]` or `waapi[]`.
   # Read its `duration_ms`. Call it D.

3. episode.start
4. episode.set_intent:
     description: "Spinner rotates 360° every Dms, infinite, with constant angular velocity."
     expected_duration_ms: D
     expected_kinds: [rotate]
     forbidden_kinds: [fade]   # spinner shouldn't fade in/out unexpectedly

5. frame.capture_series:
     # 8 samples over 1.5 periods catches start, end, and the second loop start.
     target_times_ms: [0, D*0.125, D*0.25, D*0.5, D*0.75, D, D*1.125, D*1.5]
     # (substitute actual numbers)

6. motion.contact_sheet
   → Read it. You should see the spinner orientation cycling.
   → If frames 0 and D look identical AND frame D*0.5 looks identical to them, the spinner is stuck.

7. motion.assess
   → smoothness ≥ 0.7 expected. jank_events should be empty.
```

## Stuck-spinner diagnosis

```yaml
frame.dom_query: "getComputedStyle(document.querySelector('.spinner')).animationPlayState"
  → expected: "running"
  → if "paused": find the rule that pauses it (often a parent .is-loading-paused class or a media query)

frame.dom_query: "getComputedStyle(document.querySelector('.spinner')).animationDuration"
  → expected: "<Dms>" matching CSS
  → if "0s": the animation property is being overridden

frame.dom_query: "document.querySelector('.spinner').getAnimations().map(a => ({ name: a.animationName, playState: a.playState, currentTime: a.currentTime }))"
  → WAAPI introspection. Shows actual current state.
```

## End-point identity trap

If `frame[0]` and `frame[last]` look identical, that is normal for a complete loop (1 period). Always include at least one off-cycle sample (e.g. `D*0.5`) to confirm the animation actually moved through intermediate states.
