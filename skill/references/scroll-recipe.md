# Recipe: scroll-driven animations (parallax, pin, scrub)

Scroll-driven animations don't move with the virtual clock. They move with **scroll position**. The right axis is scroll progress in [0..1], not milliseconds.

## Quick path (use this unless you have a reason not to)

```yaml
1. session.launch(url)
2. session.capabilities
3. episode.start
4. episode.set_intent:
     description: "Hero scales 1.0→2.5 and rotates 360° across the section."
     expected_kinds: [scale, rotate, color-change]
     expected_targets: ["#hero"]

5. recipes.scroll_animation_check:
     progresses: [0.0, 0.25, 0.5, 0.75, 1.0]
     target_selectors: ["#hero", ".pin-section"]
     settle_ms: 80
```

That single call returns `series` (per-progress frames + actual scroll Y), `contact_sheet` (one PNG to Read), and `assessment` (smoothness, jank, motion kinds, intent diff).

## When to drop to low-level

- You want to sample non-uniformly (e.g. 0, 0.05, 0.1, 0.4, 0.9 to focus on pin start) → `scroll.series` directly
- You want to inspect Lenis / GSAP ScrollTrigger state at each progress → interleave `frame.dom_query` calls:
  - `dom_query("window.lenis && window.lenis.scroll")` → smooth-scroll current position
  - `dom_query("window.gsap && window.gsap.globalTimeline._time")` → GSAP master time
  - `dom_query("Array.from(ScrollTrigger.getAll()).map(t => ({ id: t.vars.id, progress: t.progress, isActive: t.isActive }))")` → all ScrollTrigger states
- You want to drive scroll AND advance the virtual clock (e.g. for media that auto-plays after the scroll trigger):
  ```yaml
  trigger.scroll(y: <px>)
  clock.advance(delta_ms: 400)
  frame.capture(layout: { selectors: ["#hero"] })
  ```

## settle_ms tuning

- Lenis smooth-scroll: `settle_ms: 100-150` to let the smoothing finish
- ScrollTrigger `scrub: 0.6`: `settle_ms: 600+` because scrub itself has lag
- Pure CSS scroll-snap or instant scroll: `settle_ms: 30` is fine

## Common failures

| Symptom | Likely cause |
|---|---|
| `scroll_positions[i] == scroll_positions[i-1]` | Document is not scrollable, or `position: fixed` ancestor is stealing scroll |
| `detected_motion_kinds: [translate]` only, no scale | The animation is bound to `window.scroll` events but ScrollTrigger has not initialized - check `dom_query("typeof ScrollTrigger")` |
| `intent_match.expected_kinds_missing: [scale]` | Likely `transform: scale(...)` is being computed but a parent `overflow: hidden` is clipping - check `layout_snapshot.anomalies` for `content-clipped` |
