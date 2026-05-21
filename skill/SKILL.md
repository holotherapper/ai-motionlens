---
name: ai-motionlens
description: Verify time-dependent UI on the web. **Read this BEFORE you reach for Playwright or any single browser screenshot to check an animation, transition, scroll-driven effect, hover, loader, parallax, cursor lerp, marquee, or anything that moves over time.** Playwright captures one frame of a continuous process — that is structurally insufficient for animation quality. This skill exposes an Animation Evidence Gate (`mcp__ai-motionlens__motion_verify`) that takes a MotionContract (url + intent + triggers + sample plan + thresholds), freezes the page virtual clock, runs the full observe / score / report loop, and returns a pass/fail report with smoothness, jank events, intent_match, coverage_score, and evidence_missing. Trigger phrases: "verify the animation", "check the transition", "is the scroll smooth", "is the hover lerp right", "is the loader timing correct", "browser_take_screenshot after click/scroll/hover", "scrollTo({behavior:'instant'}) then screenshot", "setTimeout / sleep then screenshot", "getBoundingClientRect to read final state". If you are about to inspect time-dependent UI with Playwright or a single screenshot, switch to this skill instead. The fastest path is to delegate to the `animation-qa` sub-agent, which has only ai-motionlens tools and runs `motion.verify` as its primary action.
---

# ai-motionlens

**Playwright captures the current frame. ai-motionlens captures the time axis.**

Both tools drive a Chromium page. The difference is that Playwright runs in real wall-clock time and gives you whatever frame the browser happened to be on; ai-motionlens freezes a virtual clock, advances it by exact milliseconds, and returns a deterministic frame at every step. If the thing you are verifying *moves over time*, Playwright is the wrong tool.

## STOP — are you about to do this?

If any of these match what you are about to do, STOP and switch to the minimum loop below.

- `browser_take_screenshot` after `browser_click` / `browser_hover` / `browser_evaluate` that scrolled → STOP
- `scrollTo({ behavior: 'instant' })` or `window.scrollTo(0, N)` to "see what's there" → STOP
- `setTimeout` / `sleep` / `await new Promise(r => setTimeout(r, N))` to wait out an animation, then screenshot → STOP
- Reading the final state of a CSS transition / animation via `getBoundingClientRect` / `getComputedStyle` and declaring it correct → STOP
- "Loaders / hero parallax / cursor lerp / marquee / scroll reveal works because the final screenshot looks right" → STOP

All of these observe **one discrete frame of a process that takes time**. The frame you didn't capture is exactly where the bug lives (Loader half-state, parallax mid-tween, lerp overshoot, marquee seam, stagger mistiming).

## The primary path — one tool call

`motion.verify` runs the entire Animation Evidence Gate end-to-end. One MCP call, one report.

```jsonc
{
  "url": "http://localhost:5173/",
  "viewport": { "width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": true },
  "episode_intent": {
    "description": "Hero parallax fades + translates between scroll 0 and 1000 px.",
    "expected_duration_ms": 800,
    "expected_kinds": ["fade", "translate"],
    "expected_targets": ["#hero"],
    "forbidden_kinds": ["disappearance"]
  },
  "triggers": [
    { "at_t_ms": 0, "kind": { "click": { "target": { "selector": "#open-modal", "frame_path": [] } } } }
  ],
  "sample_plan": {
    "target_times_ms": [0, 60, 150, 300, 500, 800],
    "include_layout": true,
    "focus_selectors": ["#hero"]
  },
  "thresholds": {
    "min_smoothness": 0.6,
    "max_jank_events": 0,
    "require_intent_match": true,
    "min_coverage_score": 0.6,
    "require_non_static": true
  },
  "include_contact_sheet": true,
  "include_video": null
}
```

**Threshold defaults / when to relax**: `min_smoothness` 0.6, `max_jank_events` 0, `require_intent_match` true, `min_coverage_score` 0.6, `require_non_static` true are sensible defaults but **two of them are commonly relaxed**: (1) **`min_coverage_score: 0.6` is auto-skipped on rAF / GSAP / Framer Motion / Lottie pages** — `coverage_score` is `sampled_ts` divided by the union of CDP-known motion sources (CSS animation / transition / WAAPI). A JS-driven animation registers no CDP source the coverage model can see, so the score is structurally 0. When there's no CDP-known source the `coverage` gate is auto-skipped and `evidence_missing` carries a single line explaining why — you don't have to set `min_coverage_score: 0` manually for JS-driven pages anymore. (2) **`require_non_static: false` for late-tail / hover-only contracts** — a CSS transition that has already settled when the first sample fires (e.g. a 60ms ease-out hover with samples at 80/120/160ms) produces ~0 per-frame pixel delta and trips the non-static correctness gate even though the easing fit clearly identifies the motion. Use this when you're verifying the *steady state* shape of a quick transition, not its onset. Everything else (`min_smoothness`, `max_jank_events`) is reliability:`advisory` — `passes:false` from those alone is not a defect, it's a sampling / sequencing concern. The `verdict_human` line spells out which gates fired and which reliability they have, so an advisory-only fail does not require a code change.

**`expected_targets` accepts any CSS selector and is graded by `Element.matches()`, not by string equality.** `motion.verify` passes your `expected_targets` straight into the layout probe as `match_selectors`, so a contract entry like `#hero-lede` is recognised on a `<p id="hero-lede" class="hero-lede">` element even when the probe synthesizes its selector as `p.hero-lede`. The matched CSS selector is re-emitted into `moved_selectors`, so `intent_match.expected_targets_seen` lights up correctly. Use whatever selector reads cleanest in your contract (an id, a class, a descendant combinator) — the synthesized-vs-contract notation mismatch that a casual reader of just the layout snapshot worries about is already handled. Elements named in `expected_targets` also **bypass the layout probe's visibility filter**, so a hero entrance that starts at `opacity: 0` (or `display: none`, or zero bbox) is still tracked from the very first sample. The probe additionally reconciles each element's `opacity` and `transform` against `el.getAnimations()` — `layout_snapshot.elements[].opacity` is interpolated from the active animation's progress, not just the raw `getComputedStyle()` value, so a `@keyframes` fade that has finished under the paused virtual clock reads as the end-state value even when the compositor has not yet committed a frame for it. For `transform`, the same reconcile pass interpolates the **translate-only** primitive (`matrix(1,0,0,1,tx,ty)` / `translate*(...)` / `translate3d(...)`); when the keyframe also contains a rotate / scale the value falls back to the raw computed string (those components don't compose linearly). That's enough for the canonical hero-entrance pattern — `@keyframes` `transform: translateY(20px) → translateY(0)` — to surface as `axis: transform-translate-y` in `per_target_easing` and roll into `detected_motion_kinds` even when the virtual clock hasn't committed an intermediate paint.

**`appearance` vs `fade`: the classifier consults `hidden_selectors`, so the verdict is independent of `expected_targets`.** Each layout snapshot also carries `hidden_selectors` — the elements the visibility filter dropped because they were `opacity:0` / `display:none` / zero-bbox. The transition classifier reads it: a selector that was hidden on `a` and visible on `b` is `fade`, not `appearance`; the same selector seen visible-then-hidden is `fade` (out), not `disappearance`. Without this rescue the label would flip depending on whether the caller named the element in `expected_targets` (which would have kept it in `elements` via the bypass) — the same animation reading as `appearance` under `motion.suggest_intent` and `fade` under `motion.verify`. `appearance` now means **DOM-new** (the node didn't exist in the previous snapshot at all — JS-inserted element, or `display: none` without any prior presence), and `disappearance` means **DOM-removed**. A `forbidden_kinds: ["appearance"]` contract therefore catches the genuine "an element shouldn't have suddenly materialised" case without false-positives on opacity-driven entrances.

**`forbidden_kinds` is scoped to `expected_targets` when those are declared.** A contract like `expected_targets: ["#card"]` + `forbidden_kinds: ["disappearance"]` only fails when the **card itself** disappears. An unrelated `#hero` section getting scrolled out of view or unmounted by routing does not false-fail the gate — the agent's intent ("the card must not vanish") is what the gate enforces, not a page-wide ban on every DOM-removed event. When `expected_targets` is empty the gate stays page-wide, so a free-form contract still gets a ban on every DOM-removed event. The scoped selector lists are exposed as `assessment.appeared_selectors` / `assessment.disappeared_selectors` so a `forbidden-kind-observed` hint can name the actual culprit.

**Motion kind vocabulary**: `fade` (opacity > 0.01 delta), `translate` (>0.5px bbox-centre or transform-translate move), `scale` (>0.01 scale-x/y delta or >0.5px bbox-width/height), `rotate` (>1° transform-rotate delta), `color-change` (computed `color` or `background-color` change), `layout` (bbox-width/height changes — set together with `scale` when transform decomposes), `appearance` / `disappearance` (DOM-new / DOM-removed, see above), and `text-change` (element's text content rolled — `0 → 247` count-ups, typewriters, label flips). A Stats count-up that animates only the digit glyphs would otherwise leave the classifier with bbox unchanged / opacity 1 throughout, so without `text-change` the agent has no kind to declare and `intent_match` would always miss. Note that pure CSS pseudo-element (`::before` / `::after`) animation and `background-image` gradient interpolation are NOT in this vocabulary today — for those, fall back to `motion.assess`'s `per_target_easing` signal (a non-empty `per_target_easing[]` entry on the parent means *something* on or under that element moved) or assert via `motion.diff` style changes.

The response is a `MotionVerifyReport`:

```jsonc
{
  "passes": true,
  "confidence": 0.92,
  "assessment": {
    "smoothness": 0.81, "smoothness_verdict": "good", "is_static": false,
    "jank_events": [], "detected_motion_kinds": ["fade", "translate"], "moved_selectors": ["#hero"],
    "intent_match": {
      "passes": true,
      "expected_kinds_seen": ["fade", "translate"], "expected_kinds_missing": [],
      "expected_targets_seen": ["#hero"], "expected_targets_missing": [],
      "forbidden_kinds_seen": [], "duration_match": { "verdict": "match", "ratio": 1.0 }
    }
  },
  "contact_sheet": { "artifact_local_path": "...png" },
  "evidence_missing": [],
  "coverage_score": 1.0,
  "motion_sources_without_samples": [],
  "intent_targets_without_evidence": [],
  "next_required_observation": null,
  "artifact_local_path": "/.../motionlens-report.json"
}
```

`passes: true` is reliable — accept it. `passes: false` is **not** a verdict on its own: read `failed_gates[]`. Each entry has a `reliability`:

- every entry is `advisory` (`smoothness` / `jank-events` / `coverage`) → likely a measurement / choreography artifact, **not** a quality defect (a deliberate full-screen beat, a `scaleX` bar, rAF/GSAP `coverage 0`, a count→burst loader). Triage with `assessment.intent_match` + the contact sheet before acting; do not block on it.
- any entry is `correctness` (`intent-match` / `non-static`) → a real defect (the declared intent was not satisfied, or the animation never fired). Fix it.

`assessment.intent_match.passes` is the single most trustworthy signal. The rest of the report is for diagnosis when it fails.

`frames[]` does **not** carry per-frame `layout_snapshot` / `thumbnail_base64` by default. The DOM-truth they encode is already baked into the `contact_sheet` green overlay and folded into `assessment` / `coverage_score` / `diagnosis_hints` server-side — so reading the contact sheet plus those fields is the intended path, and the response stays small. To inspect one element's DOM at a moment, run the `diagnosis_hints.suggested_probe` (a `frame.dom_query` / `frame.layout_probe`). Set `include_frame_detail: true` in the contract only when you genuinely need every frame's raw layout inline. If `evidence_missing` contains `response_truncated_for_mcp_limit`, the returned `frames[]` were shrunk to fit the harness token ceiling — nothing is lost: `Read` the report's `artifact_local_path` for the complete per-frame report.

**Need a fullpage settled-state shot?** Set `include_final_fullpage_screenshot: true` in the contract. The gate captures a single PNG of the whole page after the sample plan completes and attaches it as `final_fullpage_screenshot.artifact_local_path`. Right before the shot the gate injects `*{opacity:1!important; transform:none!important; transition:none!important; animation:none!important; visibility:visible!important; filter:none!important;}` to force every IO-driven reveal / pre-translated entrance / opacity-zero baseline into its "content visible" state — `captureBeyondViewport` doesn't trigger `IntersectionObserver` on its own, and a paused virtual clock doesn't tick a 200ms reveal transition to its end, so without this override the bottom-of-page sections would read as black/empty. The injection is removed immediately after the screenshot so subsequent gates / queries see the page as it actually is.

This screenshot answers **"is the content structure of the whole page correct?"** — it's not a faithful render of any particular instant of the animation. Use the `contact_sheet` for "how did this section actually appear?" and `final_fullpage_screenshot` for "do all the sections together render with the right copy / layout / color?". Don't reach for Playwright's `browser_take_screenshot` for either question.

Page-wide ambient loops (marquee scrollers, brand-mark spinners, `orb-float`-style background drifts) are detected and **lifted out** of `unobserved_intervals[].active_sources[]` into the top-level `ambient_source_names: Vec<String>`. So when six or eight infinite-loop `@keyframes` run on the page, you see them listed once, not redundantly stuffed into every interval — the response stays sized for the actual probe-worthy sources (the one-shot entrance that started inside this window). `include_frame_detail: true` keeps the full expansion if you genuinely need it.

## When it fails: read `diagnosis_hints` first

A failing report carries a `diagnosis_hints[]` array. Each hint is a root-cause candidate the gate already inferred from the captured evidence — `code` (machine-pattern-matchable kebab-case), `target_selector`, a human `message`, the `observed` state, and **`suggested_probe`** (the exact next tool call to confirm the candidate).

```jsonc
{
  "diagnosis_hints": [
    {
      "code": "hidden-by-zero-opacity",
      "target_selector": "#hero",
      "message": "Selector `#hero` has opacity ~0 in both the first and last sampled frames. The element exists and is laid out, but no fade-in occurred. Check whether the opacity keyframe / class toggle actually fires after the trigger.",
      "observed": { "opacity_first": 0.0, "opacity_last": 0.0 },
      "suggested_probe": "frame.dom_query { selector: '#hero', expression: 'el.getAnimations().map(a => ({ id: a.id, playState: a.playState, currentTime: a.currentTime }))' }"
    }
  ]
}
```

Defined codes:
- `intent-target-parent-of-moving-element` — the declared `expected_target` exists, its own bbox / opacity / transform stayed static across the sequence, but one or more of its **descendants** moved. The agent declared the parent while the animation runs on the children — the canonical text-stagger case (`#hero-lede` declared, the `.word` children rise). Re-declare with a descendant selector (the hint lists the moving children) or list the children explicitly in `expected_targets`. **Read this first** when the page IS animating but the parent target is reported as silent — `raf-source-stalled`'s candidate-cause list won't apply (none of (a)–(c) match), so chasing it wastes a turn.
- `viewport-covered-by-overlay` — a non-shell element covers ≥85% of the viewport in every captured frame (a splash / loader / modal backdrop that was still up when the sample window started). The layout probe sees behind it, so `intent_match` may already be green — but the **`contact_sheet` PNG only shows the overlay**, so it's easy to misread "everything is broken" when really the behind-overlay animation is fine. Hide the overlay with an evaluate trigger at `at_t_ms: 0` (the hint's `suggested_probe` carries a ready-to-paste fragment) and re-run; the contact sheet will then show the real page. **This hint exists so you don't reach for Playwright** to "see what's behind" — that anti-pattern is what this skill is built to prevent.
- `selector-not-found` — expected_target absent from the final layout snapshot
- `hidden-by-zero-opacity` — element exists, opacity stayed 0 throughout
- `hidden-by-display-none` — element resolved but `display: none` / zero bbox
- `bbox-static-style-mutating` — bbox unchanged but color/opacity changed (intent expected translate/scale?)
- `no-dom-mutation-after-trigger` — trigger fired but the page never repainted differently
- `no-motion-source-registered` — `timeline.sources` is empty
- `motion-source-inactive` — sources exist but none were `active_sources` at sample time
- `expected-targets-all-missing` — every selector in `expected_targets` is absent from every captured layout AND `intent_match.expected_targets_seen` is empty (i.e. nothing in `expected_targets` was caught moving via `per_target_easing` either — really nothing observed). **Read this BEFORE any per-selector `selector-not-found` hint**: the most common meta-causes are (a) the URL is serving unexpected content (port collision / redirect / wrong build), (b) the page never reached the intended state (network failure / auth wall / JS error), (c) contract drift (selectors renamed in a refactor). Confirm the page actually rendered what you expect — `frame.dom_query` `document.title + location.href` — before chasing per-selector causes. (Suppressed when `intent_match` has seen any target — that means the target IS being tracked, just not via this snapshot.)
- `raf-source-stalled` — rAF is registered and the page IS animating, but `intent`'s expected target(s) never moved. **Read the `observed.candidate_causes` list, not the headline message, as a verdict** — the hint cannot tell which candidate applies. Rule out (b) target-not-in-DOM and (c) chained-reveal-blocked first; only then grep your source for the `performance.now()`-anchored rAF pattern (see "Animating under the virtual clock" above) and switch the time origin to the rAF `t` argument.
- `css-transition-jumped-over` — a CSS `transition:` is registered AND `is_static:true` AND consecutive samples are wider apart than the shortest transition duration. `clock.advance` skipped past the whole transition window in one step so every middle sample reads the end state — symptom-identical to "animation never fired" but the fix is different. Either shrink the `sample_plan` around the transition window (entries spaced < transition duration) OR rewrite the `transition:` as `@keyframes` (CDP picks `@keyframes` up structurally and interpolates per tick).
- `css-animation-delay-overrun` — a CSS `@keyframes` animation's `[delay, delay+duration]` active window doesn't overlap the sampled time window AND `is_static:true`. A long `animation-delay` (e.g. 2700 ms) starts from the virtual clock's t=0 pinned at navigation, so a `sample_plan` that ends before the delay reads the page as pre-animation, and one that starts after `delay+duration` reads it as fully settled — either way the run looks static even though the animation plays correctly in a real browser. Shift `sample_plan.target_times_ms` to straddle the active window (the hint's `observed.first_animation_active_window_ms`) OR shorten `animation-delay` to fit your sampled span. Third leg of the virtual-clock × time-origin trap, alongside `raf-source-stalled` and `css-transition-jumped-over`.
- `silent-static-prominent-element` — an element stayed completely still while other elements moved and `intent` expects motion, but it was not in `expected_targets` so its stillness never affected pass/fail. Fires for an element that is either visually prominent **or** effectively invisible (offset / clipped mostly out of the viewport — `observed.effectively_invisible: true`, e.g. a hero stuck at `translateY(342px)` under a mask). This is the one that catches an **undeclared element silently broken** (a hero headline that never animated): `passes` can be green and this hint still fires. If it should animate, treat it as a real defect (an `effectively_invisible` one almost always is). Document-root pseudo-elements (`html`, `body`) and near-full-viewport (>=90%) shell containers are filtered out so the hint stays noise-free.
- `jank-spike` — single transition's pixel_delta is `>= mean + 2σ`
- `intent-duration-mismatch` — `coverage_ms` doesn't match `expected_duration_ms`
- `forbidden-kind-observed` — `forbidden_kinds` was seen

Run the `suggested_probe` first — don't guess.

## Reading `jank_events` (don't fail on an intended beat)

Each `jank_events[]` entry has a `kind`; the two numbers mean different
things per kind, so never compare them to `per_transition_delta` blindly:

- `kind: "pixel-cv-outlier"` — `delta_ratio` is a changed-pixel ratio
  (same basis as `per_transition_delta`); `z_score` is `(delta-mean)/stddev`.
  A full-screen curtain / wipe / hero reveal trips this **even when it is
  the intended beat**. Before calling it a defect, look at the contact
  sheet for that interval and whether the motion matches `intent` — a
  deliberate big beat is not jank.
- `kind: "positional-teleport"` — `delta_ratio` is jump-distance /
  viewport-width; `z_score` is a multiple of the element's median move
  (>=4 = teleport), **not** a standard score. This is the one that catches
  a real discontinuity a single screenshot would miss.

If `passes:false` is driven only by a `pixel-cv-outlier` on an interval
your `intent` expected to move big, it is likely a measurement /
choreography artifact, not a quality defect — read `intent_match` + the
contact sheet; don't trust `passes` alone.

`duration_match.observed_coverage_ms` measures the **declared targets'**
actual onset→settle span when `expected_targets` are specified —
ambient background animations (a long-loop `@keyframes` drift, an
infinite marquee, a parallax orb) running in parallel don't push the
verdict to `too-long` even when you stretch `target_times_ms` past
their loop boundary. The verdict only gates the upper bound in this
mode: a `too-short` ratio is normal sampling-granularity noise (a
600ms ease-out-cubic typically reads `settle_ms` at t≈450..510ms
because that's when the first sample crosses 0.95 progress), not a
defect. When no `expected_targets` are listed (or none resolved a
motion span), the fallback is the sample_plan's full span with both
bounds gated — that's the original behaviour for free-form contracts.

## Per-target easing / stagger / overshoot / runtime jank

`assessment.per_target_easing[]` carries a best-fit easing template per moving selector (linear, ease-in, ease-out, ease-in-out, ease-in-cubic, ease-out-cubic, ease-in-out-cubic) with an RMS residual. Use this when the design spec calls for a specific easing — declare `intent.expected_easing` so `intent_match.easing_match` grades it for you. `stagger_uniformity` quantifies the uniformity of onset times across multiple targets. `overshoot_events` flags targets that went past their final value before settling (intentional spring bounce vs. unintended overshoot bug). `runtime_jank_events` carries Chrome's Long-Animation-Frame entries — real-time main-thread blocking, not synthesised from pixel deltas.

`detected_motion_kinds` is reconciled with `per_target_easing` after the per-frame-pair classification step. The per-pair classifier uses a 0.5px movement threshold (so it ignores subpixel drift / font remetering), which can miss a sharp CSS `transition` whose samples all land in the curve's tail (e.g. observed values `[-9.35, -9.74, -9.94, -10, -10]` — every adjacent delta is under 0.5px but the sequence range is 10px). In that case the easing fit still resolves the dominant axis correctly, and the resolver folds the axis back into `detected_motion_kinds` (`transform-translate-*` / `translate-*` → `translate`, `opacity` → `fade`). So `expected_kinds: ["translate"]` does not fight with `expected_kinds_missing: ["translate"]` when the easing fit clearly identified the same translate-y on the same sequence.

`is_static` is reconciled the same way. The first-pass verdict is the page-wide pixel-delta CV — a hero word stagger that translates 100px on a 1280x800 viewport produces a mean ~2e-5, well under the `NO_MOTION_FLOOR` threshold, so the page-wide signal alone reads as "static". When `per_target_easing` has fit *any* axis with `mag > 1e-3`, that DOM-truth observation overrides the pixel-CV verdict and `is_static` flips to false — the `non-static` correctness gate does not fire on a motion the rest of the report (`intent_match`, `per_target_easing`, contact sheet) has clearly captured. `smoothness` and `smoothness_verdict` stay anchored to the page-wide CV signal as an advisory ("the page as a whole isn't moving much"), so a small-bbox motion still surfaces a low smoothness but doesn't fail correctness.

**Resolving a hashed `target_selector` in `active_sources` / `timeline_sources`**: The CDP Animation domain returns `cssId` as a stable selector (`"#hero-cta"`) when the animated element has a unique id, but as a backend-node-id hash (`"tt2ToBzbmOvcjA=="`) when it doesn't. `motion.verify` does the cross-reference for you most of the time: when exactly one element on the page is running the matching `@keyframes`, the source's `target_selector_hint` field carries the resolved selector — read that first. The raw cross-reference data stays available so you can disambiguate the staggered cases: (a) `active_sources[].name` carries the `@keyframes` name, and (b) `layout_snapshot.elements[].running_animations` lists each element's running animation names. When multiple elements share the same `@keyframes` (a stagger), `target_selector_hint` is intentionally left `None` so you don't misread an arbitrary pick as authoritative — combine with `moved_selectors` to identify the specific element. **When you control the markup, give animated elements a unique `id`** (or write `@keyframes` against an id-scoped selector like `#hero-cta`) — CDP will then return that id verbatim and no cross-reference is needed at all.

## Reaching CSS `:hover` and `pointer*` listeners

JS-mode triggers (the default) can't activate the `:hover` pseudo-class — they dispatch DOM events synchronously but don't move the compositor cursor. When the animation depends on `:hover` or a coordinate-driven listener, set `input_mode: "cdp"` on the trigger:

```jsonc
{
  "at_t_ms": 0,
  "kind": { "kind": "hover", "target": { "selector": "#card", "frame_path": [] } },
  "input_mode": "cdp"
}
```

CDP-mode dispatches a real `Input.dispatchMouseEvent` through the compositor pipeline **and** issues a `CSS.forcePseudoState(['hover'])` against the same element via the style engine, so the `:hover` pseudo-class is guaranteed to apply (the dispatchMouseEvent alone doesn't always cause Chromium's style engine to re-apply hover rules under a paused virtual clock). The virtual clock auto-advances 16ms (one paint flush) so the next `frame.capture` doesn't hang on a pending paint.

That auto +16ms means a `sample_plan.target_times_ms` entry at the **same** virtual time as a CDP trigger has already been overshot by the time the sampler reaches it. `motion.verify` does NOT die on this — it captures at the current time (`at_or_after` semantics) and adds `sample_plan_drift_after_cdp_trigger: ...` to `evidence_missing` naming the original target and the actual capture time. If you need the original target hit exactly, pad your sample by `>=16ms` after any `input_mode:"cdp"` trigger.

**`:hover` transitions are a known limitation under the paused virtual clock**: even with the pseudo-class active and the style change committed, a CSS `transition: background-color 200ms ease` started by the `:hover` activation **does not tick its `currentTime` from `clock.advance`** — Chromium's transition scheduler is not pinned to `document.timeline` the way `@keyframes` is. The static (non-transitioned) hover declaration takes effect immediately; the transition's mid-curve values do not. If you need to verify the hover motion itself (the actual 200ms ramp), write the hover effect as a class-driven `@keyframes` (or a JS class toggle that arms a `@keyframes` animation) — CDP picks `@keyframes` up structurally and the existing opacity / transform reconciliation interpolates from `getAnimations()[].effect.getKeyframes()`. If you only need to verify the **end state** of a hover, the current behaviour is sufficient: read the computed style after the trigger and confirm the hovered value is applied.

## Animating under the virtual clock (don't anchor on `performance.now()`)

`ai-motionlens` freezes the page virtual clock and advances it deterministically. `requestAnimationFrame` callbacks fire as expected, BUT **`performance.now()` does not advance in lock-step with the virtual rAF tick**. A standard JS loop like

```js
const start = performance.now();                  // wall-time anchor, not virtual
requestAnimationFrame(function loop() {
  const dt = performance.now() - start;           // stays near 0 under the virtual clock
  // ... never reaches its end condition
});
```

stalls forever under verification, and any chained reveal that depends on it (loader → `classList.add('ready')` → hero entrance) stays dead too. Use the rAF callback's `t` argument as the time origin instead:

```js
let start;
requestAnimationFrame(function loop(t) {
  if (start === undefined) start = t;
  const dt = t - start;                           // advances with the virtual clock
  // ...
});
```

If `motion.verify` emits a `diagnosis_hints` entry with `code: "raf-source-stalled"` and your `intent` targets are listed as stuck while the page IS animating something else, this pattern is the most likely cause. Switch the time origin to the rAF `t` argument and re-run.

### CSS `transition:` also has a virtual-clock trap

A CSS `transition:` does NOT generate per-tick interpolation events the way `@keyframes` does. When `clock.advance` jumps over the whole transition window in one step, the renderer skips straight to the end state and your sampled frames in the middle read `getComputedStyle()` as "already settled". Every middle sample looks identical, the sequence is judged `is_static: true`, and the `non-static` correctness gate fails — even though, in real time, the element really does animate.

Two reliable fixes:

- **Sample inside the window**. Pick `target_times_ms` such that consecutive samples are *smaller* than the transition's `duration`. e.g. for a `transition: opacity 600ms`, sample every 100–200 ms across that span, not in one 1500 ms jump.
- **Use `@keyframes`** instead of `transition:` for anything you want `motion.verify` to grade end-to-end. The Animation domain picks `@keyframes` up structurally via CDP, so the sampler can interpolate the right intermediate values regardless of how coarsely you sampled.

The symptom to look for: `assessment.is_static: true` AND `failed_gates` contains `non-static (correctness)` AND the page does animate when you open it in a real browser. That combination is almost always a `transition:` jumped over by a coarse `clock.advance`.

### A long `animation-delay` is the third leg of the same trap

The virtual clock is pinned at `t=0` from navigation, so a CSS `@keyframes` animation with a long `animation-delay` (e.g. 2700 ms) has its active window `[delay, delay+duration]` measured from that pin. If your `sample_plan.target_times_ms` ends before `delay`, every sample sees the pre-animation state. If your sample plan starts after `delay+duration`, every sample sees the settled state. Either way the sequence comes back `is_static: true` even though the animation plays fine in a real browser — `motion.verify` emits `css-animation-delay-overrun` for exactly this case and tells you the active window in `observed.first_animation_active_window_ms`.

Two fixes:

- **Sample inside the active window**. Adjust `sample_plan.target_times_ms` so consecutive entries straddle `[delay, delay+duration]`.
- **Shorten `animation-delay`** to fit your sampled span — long author-time delays exist for choreography, but if `motion.verify` is meant to grade the animation itself, the delay can usually be expressed as a different timing primitive (later `target_times_ms`, a trigger-armed class toggle, or a master timeline with explicit offsets).

## Deterministic replay (`external_state`)

When the page reads `Math.random` / `crypto.getRandomValues`, depends on the local timezone / locale / user-agent, or persists state to cookies / localStorage, configure `external_state` on `session.launch` (or in the contract for `motion.verify`):

```jsonc
{
  "external_state": {
    "random": "pinned", "crypto": "pinned",
    "storage": "pinned",
    "timezone": "pinned", "locale": "pinned", "user_agent": "pinned",
    "network": "live", "service_worker": "live", "web_socket": "live",
    "third_party_iframes": "live", "media_devices": "disabled"
  },
  "external_state_seed": 42
}
```

`random.Pinned` reseeds `Math.random` and `crypto.getRandomValues` from `external_state_seed` so two launches with the same seed produce identical sequences. `storage.Pinned` clears cookies / localStorage / sessionStorage at launch. `timezone` / `locale` / `user_agent` Pinned switch to UTC / en-US / a fixed reproducible UA so the same gate reports the same numbers across machines.

## Always probe first with `motion.suggest_intent` — even when you wrote the animation yourself

`motion.verify` grades **observed** motion against the `episode_intent` you declare, and the most common reason it takes 2–3 verify rounds to reach `passes:true` is a mismatch between what you *think* will happen and what the renderer actually does (drift between source `expected_duration_ms` and the real settled time, a selector that animates via a child rather than the declared parent, an `expected_kinds` list missing `fade` / `translate` / `layout`). So **call `motion.suggest_intent` first regardless** — it runs a short probe, classifies what moved, infers a duration, and hands back an `EpisodeIntent` draft you tighten and paste into the contract. This skips 1–2 verify retries that would otherwise be spent fighting the contract instead of the implementation:

```jsonc
{
  "url": "http://localhost:5173/",
  "viewport": { "width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": true },
  "triggers": [
    { "at_t_ms": 0, "kind": { "click": { "target": { "selector": "#open-modal", "frame_path": [] } } } }
  ],
  "probe_window_ms": 480,
  "probe_steps": 6
}
```

Response carries `suggested_intent` (a ready-to-paste `EpisodeIntent`), `detected_motion_kinds`, `moved_selectors`, `smoothness`, `is_static`, and `notes` ("animation appears to settle around 320ms", "no motion detected — widen the probe", etc.). Treat it as a draft — tighten `description`, drop selectors you don't own, add `forbidden_kinds` — then run `motion.verify`. When `is_static: true` comes back, the URL / trigger / probe is wrong; don't paste an empty intent and hope.

`focus_selectors` here works the same way `expected_targets` works in `motion.verify`: the selectors you list are treated as observer-named "watch these" hints and **bypass the layout probe's visibility filter**, so a hero entrance starting at `opacity: 0` still seeds the t=0 sample and the fade-in registers as real motion. Without that, narrowing focus to an `opacity:0`-armed element would return `is_static: true` (the probe never sees the baseline) — a confusing false negative on the canonical hero pattern. So feel free to narrow `focus_selectors` to the elements you actually care about; the bypass is automatic.

## Even faster — delegate to `animation-qa`

`animation-qa` is a sub-agent whose tools allowlist is `mcp__ai-motionlens__*` only — it physically cannot reach for Playwright. Use it when you don't want to construct the contract yourself:

```text
Agent({
  subagent_type: "animation-qa",
  prompt: "Verify the page at http://localhost:5173/ — hero parallax should fade + translate between scroll 0..1000 px. Return the report."
})
```

The sub-agent picks viewport, samples the timeline sources, synthesizes the contract, runs `motion.verify`, and returns the summary.

## Escalation path — when the high-level call isn't enough

Drop to the low-level tools only when a single contract isn't sufficient:

| Group | Tool | Use when |
|---|---|---|
| audit | `motion.audit_required` | Decide whether the page has motion *before* deciding to verify. Returns `{ motion_detected, sources, required_verification, recommended_target_times_ms, reason }`. Designed for CI / sub-agent use. |
| session | `session.launch` / `close` | Reuse a session across many verifies on the same page |
| | `session.capabilities` | Read the driver's `external_state` and `replay_cost` |
| episode | `episode.start` / `set_intent` / `replay_to` | Step-by-step assembly when `motion.verify` is too coarse |
| clock | `clock.advance` / `clock.status` | Move the live virtual clock forward |
| trigger | `trigger.click` / `hover` / `type` / `scroll` / `evaluate` | Drive interactions recorded into the episode (does NOT consume virtual time) |
| frame | `frame.capture` / `frame.capture_series` / `frame.layout_probe` / `frame.dom_query` / `frame.bisect` | Individual frame operations; `bisect` for the largest unobserved interval |
| library | `library.gsap_state` / `library.lenis_state` / `library.lottie_state` | Structured introspection of the three most common motion libraries |
| motion | `motion.diff` / `motion.assess` / `motion.contact_sheet` / `motion.video_export` | The pieces `motion.verify` composes internally |
| timeline | `timeline.sources` | Inventory of CSS / WAAPI / rAF / timer / media |
| recipes | `recipes.scroll_animation_check` | Scroll-driven contract on a progress axis (0..1). Self-starts an episode if the caller didn't. **Watch for `sticky` + pinned sections**: the recipe samples progress against the section's bbox-viewport intersection, but a `position: sticky` (or GSAP-pinned) child has its own active range that doesn't align with the bbox progress. That mismatch surfaces as `scroll_range_match.verdict: narrower-than-expected` and emits an *advisory* `scroll-range-match` gate (not correctness) — narrow your `progresses` argument or `expected_progress_range` to match the real pin/release window. |
| evidence | `evidence.list` / `append_observation` | Ledger inspection / agent observation log |
| scroll | `scroll.series` | Per-progress-step captures for scroll-timeline UI |

You don't need to memorize this table. `motion.verify` and the `animation-qa` sub-agent cover ~95% of cases; the rest is escalation.

## Frames are files, not JSON

`Frame.artifact_local_path` is the on-disk path. Just `Read` it. Do not try to parse `thumbnail_base64` unless you explicitly asked for `thumbnail: true` (default off). For animation overviews, prefer `motion.contact_sheet` (included by default in `motion.verify`) over reading frames one by one — one horizontal PNG conveys what 6 thumbnails do, in fewer image tokens.

The full-resolution image is also reachable via MCP `resources/read` on `artifact_uri` (`motionlens://artifacts/<session>/<filename>`); use this when the local path isn't accessible (different machine / remote agent).

## Detailed recipes (read on demand)

When the task fits one of these patterns, read the matching reference file:

- Modal / drawer / popover open-close → `references/modal-recipe.md`
- Scroll-driven (parallax, pin, scrub) → `references/scroll-recipe.md`
- GSAP timeline / Lenis / Lottie / Motion → `references/library-tips.md`
- Entrance / exit micro-interactions → `references/entrance-recipe.md`
- Loops (spinners, marquees, infinite text scrollers) → `references/loop-recipe.md`
- Trigger timing model, `wait_policy` choice, `external_state` interpretation, `evidence` usage, replay cost → `references/internals.md`

## Anti-patterns

- One-frame screenshots after a click / scroll / hover. They lie.
- Skipping intent. `motion.verify` falls back to a synthesized intent from `timeline.sources`, but you should declare your own when you know the design — that's the bar `intent_match` grades against.
- Trusting your own vision unconditionally. If the report shows `layout` anomalies or `intent_targets_without_evidence`, something moved that your eye missed (or *didn't* move that you assumed did).
- Calling Playwright `browser_take_screenshot` to check an animation. That is the exact failure case this skill replaces.

## Mindset

You will be confidently wrong about animations on the first iteration. Static screenshots make the page look fine. This skill exists so you disprove your own first impression — by observing the time axis — before declaring done.
