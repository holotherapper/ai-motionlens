# Recipe: entrance / exit micro-interactions

Buttons that pulse on hover, cards that fade in on mount, toasts that slide in - these are short (50-400ms), repeated all over an app, and notoriously where vision-only checks fail.

## Steps

```yaml
1. session.launch(url)
2. session.capabilities
3. episode.start
4. episode.set_intent:
     description: "Toast slides in from top-right over ~200ms, ease-out."
     expected_duration_ms: 200
     expected_kinds: [translate, fade]
     expected_targets: [".toast"]

5. trigger.evaluate(js: "window.showToast && window.showToast('hello')")
   # or whatever your app's API is. Triggers are recorded so bisect works.

6. frame.capture_series:
     target_times_ms: [0, 30, 70, 130, 200, 320]
     layout: { selectors: [".toast"] }

7. motion.contact_sheet(frame_ids: <all>)
8. motion.assess(frame_ids: <all>)
```

## For hover-driven entrances

`trigger.hover` dispatches `mouseenter` / `mouseover`. After hover, capture:

```yaml
trigger.hover(selector: ".btn-primary")
frame.capture_series:
  target_times_ms: [0, 30, 80, 150, 250]
  layout: { selectors: [".btn-primary"] }
```

`mouseenter` may trigger a CSS transition. The `capture_series` sees the visual progression.

## Things to watch in entrance animations

- **Initial flash**: `t=0` frame should already show the starting opacity/transform, not the final state. If the contact sheet shows the toast fully visible at t=0, the animation is being skipped (likely because the element was already in the final state before the trigger fired).
- **Overshoot**: spring-based entrances may overshoot. That is intentional - but `motion.assess` will flag the overshoot as a `jank_event`. Check `intent_match.passes`: if the springiness is intended, it should be in `expected_kinds`.
- **Late paint**: if `t=30` looks identical to `t=0` but `t=70` jumps to mid-animation, the browser took ~50ms to start animating. This is usually a CSS specificity or class-add-then-class-remove race; use `dom_query` to inspect `getComputedStyle(...).transitionDelay`.
