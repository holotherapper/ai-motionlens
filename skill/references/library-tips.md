# Library tips - GSAP / Lenis / Motion / Lottie

`ai-motionlens` ships three high-confidence inspectors for the most common
JavaScript animation libraries. Prefer them over hand-rolled `frame.dom_query`
because they return structured JSON the agent can pattern-match on.

| Library | Tool | Returns |
|---|---|---|
| GSAP | `library.gsap_state` | `{ available, master: { time, progress, totalDuration, paused }, scroll_triggers: [{ id, progress, isActive, start, end, scrub, pin }] }` |
| Lenis | `library.lenis_state` | `{ available, scroll, native_scroll_y, velocity, is_smooth, is_stopped }` |
| Lottie | `library.lottie_state` | `{ available, registered: [{ name, currentFrame, totalFrames, isPaused, isLoaded, playSpeed }] }` |

Each of these is **read-only** and does not append to the episode ledger.
If you need to mutate state (e.g. `ScrollTrigger.refresh()`), use
`trigger.evaluate` so the change is recorded and replayable.

## When the inspector says `available: false`

The library isn't loaded on the page. Common causes:

- The `<script>` tag has `defer` / `async` and the agent looked too early →
  call `clock.advance(50)` or `frame.capture_series` past the script's
  parse-and-execute window, then re-call the inspector.
- A different global name (older GSAP exposes `TweenMax` instead of `gsap`,
  some Lottie bundles use `lottie` instead of `bodymovin`) → fall through to
  `frame.dom_query` with `typeof window.<expected>` to confirm.

## Falling through to `frame.dom_query`

For libraries the inspectors don't cover (Motion / Framer Motion, Anime.js,
custom rAF loops), drop to `frame.dom_query` with the most direct probe:

```yaml
# Motion / Framer Motion: there is no global. Use WAAPI introspection.
frame.dom_query(js: "Array.from(document.querySelectorAll('*')).flatMap(el => el.getAnimations()).map(a => ({ tag: a.effect && a.effect.target && a.effect.target.tagName, name: a.animationName || '(waapi)', currentTime: a.currentTime, playState: a.playState, duration: (a.effect && a.effect.getTiming && a.effect.getTiming().duration) }))")

# Anime.js: instances are not exposed by default; the agent must surface them
# from user code. Anchor with selector-based probes instead.
frame.dom_query(js: "getComputedStyle(document.querySelector('#hero')).transform")
```

`frame.dom_query` is also the right tool when the library inspector returns
fewer fields than the agent needs (e.g. ScrollTrigger's `vars.toggleActions`).

## Pairing with `timeline.sources`

If `library.gsap_state` says GSAP isn't loaded but `motion.assess`
shows motion, the animation likely comes from CSS or WAAPI. Call
`timeline.sources` -- `css_animations` / `css_transitions` / `waapi` carry
authoritative CDP-detected items with `confidence: 0.95`.

## Lenis-specific note on `scroll.series`

When Lenis is active, `trigger.scroll(y: N)` writes `window.scrollY` instantly
but Lenis catches up over a few frames. Increase the `settle_ms` argument on
`scroll.series` (default 50ms → bump to 100-150ms) so the pinned / scrubbed
elements settle before each capture.
