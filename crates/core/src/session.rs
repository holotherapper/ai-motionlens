//! Session + Episode + Evidence ledger.
//!
//! A `Session` owns one Chromium browser and one primary page, plus a
//! [`VirtualTimeDriver`] (the clock) and a [`CdpTimelineCollector`] (the
//! inspector).
//!
//! An `Episode` is a recording: an `EpisodeStartState`, an ordered list of
//! `TriggerEvent`s annotated with the virtual timestamps they fired at, and a
//! growing `EvidenceLedger`. The live page advances forward; replaying the
//! past happens on a scratch page so the live state is never rewound.

use std::sync::Arc;
use std::time::{Instant, SystemTime};

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::animation::{
    EnableParams as AnimationEnableParams, EventAnimationStarted,
};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::handler::viewport::Viewport;
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide::Page;
use futures::StreamExt as _;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::warn;

use crate::artifact::ArtifactStore;
use crate::driver::{
    AnimationBuffer, CdpTimelineCollector, TimeDriver, TimelineSourceCollector, VirtualTimeDriver,
};
use crate::error::{Error, Result};
use crate::model::{
    AdvanceOutcome, AdvanceResult, AnimationAssessment, BboxChange, BisectReplayKind, BisectResult,
    CaptureSeriesResult, ClockStatus, ContactSheet, ContactSheetCell, ContractTrigger,
    DiagnosisHint, DomQueryResult, DurationMatch, EpisodeId, EpisodeIntent, EpisodeStartState,
    EvidenceLedger, ExternalStatePolicy, Frame, FrameId, ImageFormat, InputMode, IntentMatchReport,
    Interval, JankEvent, JankKind, LaunchOptions, LayoutProbeOptions, LayoutSnapshot,
    MotionAuditResult, MotionCategory, MotionContract, MotionDiff, MotionSuggestIntentRequest,
    MotionSuggestIntentResponse, MotionVerifyReport, Observation, ObservationId, RecipeInclude,
    ReplayResult, ScrollAnimationCheckResult, ScrollRangeMatch, ScrollSeriesResult,
    SectionProgressSpec, SessionCapabilities, SessionId, StyleChange, TargetSpec, TimelineSources,
    TriggerEvent, TriggerEventId, TriggerKind, UnobservedInterval, WaitOutcome, WaitPolicy,
};

const DEFAULT_MAX_STARVATION_COUNT: u32 = 100;

/// Maximum number of visible elements included in a layout snapshot. Anomalies
/// are reported separately and are not capped.
const LAYOUT_PROBE_MAX_ELEMENTS: usize = 300;

/// Soft timeout applied to every `WaitPolicy` honor call. The virtual clock is
/// usually paused while a wait policy is honored, which means `rAF` /
/// `networkAlmostIdle` may never fire. The timeout guarantees the agent loop
/// can always make progress.
const WAIT_POLICY_TIMEOUT_MS: u64 = 1_500;

/// Threshold under which a mean per-frame pixel delta is treated as "no motion
/// at all". Below this, `motion.assess` sets `is_static: true` and
/// `smoothness_verdict: "no-motion"` so callers can distinguish a perfectly
/// still page from a perfectly smooth animation.
const NO_MOTION_FLOOR: f64 = 1e-4;

const LAYOUT_PROBE_JS_BODY: &str = r#"
  const opts = arguments[0] || {};
  const MAX_ELEMENTS = (typeof opts.max_elements === 'number') ? opts.max_elements : 300;
  const SELECTOR_FILTERS = Array.isArray(opts.selectors) ? opts.selectors : null;
  const IGNORE_SELECTORS = Array.isArray(opts.ignore_selectors) ? opts.ignore_selectors : [];
  const MATCH_SELECTORS = Array.isArray(opts.match_selectors) ? opts.match_selectors : [];
  const IGNORE_ANOMALIES = new Set(Array.isArray(opts.ignore_anomalies) ? opts.ignore_anomalies : []);
  const vw = window.innerWidth;
  const vh = window.innerHeight;
  const doc = document.documentElement;
  // Cap on `hidden_selectors` so a `display:none` design system page
  // (hundreds of hidden helpers) cannot blow up the payload. Selectors
  // alone are kept — no bbox / opacity / transform.
  const MAX_HIDDEN_SELECTORS = 200;
  const result = {
    viewport_width: vw,
    viewport_height: vh,
    document_scroll_width: Math.max(doc.scrollWidth, document.body ? document.body.scrollWidth : 0),
    document_scroll_height: Math.max(doc.scrollHeight, document.body ? document.body.scrollHeight : 0),
    scroll_x: window.scrollX || 0,
    scroll_y: window.scrollY || 0,
    element_count: 0,
    elements_returned: 0,
    truncated: false,
    elements: [],
    anomalies: [],
    hidden_selectors: [],
  };

  function makeSelector(el) {
    if (el.id) return '#' + el.id;
    const tag = el.tagName.toLowerCase();
    if (el.classList && el.classList.length) {
      const cls = Array.from(el.classList).slice(0, 2).join('.');
      return tag + '.' + cls;
    }
    return tag;
  }

  function rectsOverlap(a, b) {
    return a.x < b.x + b.w && a.x + a.w > b.x && a.y < b.y + b.h && a.y + a.h > b.y;
  }

  function matchesAnySelector(el, list) {
    for (const sel of list) {
      try { if (el.matches(sel)) return true; } catch (_) {}
    }
    return false;
  }

  function matchedSelectors(el, list) {
    const hit = [];
    for (const sel of list) {
      try { if (el.matches(sel)) hit.push(sel); } catch (_) {}
    }
    return hit;
  }

  function ancestorMatchedSelectors(el, list) {
    // Strict ancestor: start from parentElement so the element's own
    // match (covered by matchedSelectors above) is never duplicated
    // here. Used by `intent-target-parent-of-moving-element` to spot
    // "agent declared the parent; the animation runs on the child".
    const hit = [];
    const par = el.parentElement;
    if (!par) return hit;
    for (const sel of list) {
      try { if (par.closest(sel)) hit.push(sel); } catch (_) {}
    }
    return hit;
  }

  function pushAnomaly(kind, payload) {
    if (IGNORE_ANOMALIES.has(kind)) return;
    result.anomalies.push(Object.assign({ kind: kind }, payload));
  }

  const all = document.querySelectorAll('*');
  const visibles = [];
  for (const el of all) {
    const r = el.getBoundingClientRect();
    const cs = getComputedStyle(el);
    // Elements named in MATCH_SELECTORS (typically `intent.expected_targets`
    // forwarded by motion.verify) bypass the visibility filter so a
    // hero entrance that starts at opacity:0 / clipped / zero-sized is
    // still observable across the sequence. Without this rescue the
    // first sample drops the element entirely and motion.verify reports
    // `selector-not-found` even though the animation runs (the canonical
    // case is an eyebrow / lede that fades in from opacity:0 + a 20px
    // translate).
    const isExpected = MATCH_SELECTORS.length && matchesAnySelector(el, MATCH_SELECTORS);
    if (!isExpected) {
      const dropZeroBox = r.width === 0 && r.height === 0;
      const dropNoneHidden = cs.display === 'none' || cs.visibility === 'hidden';
      const dropZeroOpacity = parseFloat(cs.opacity) === 0;
      if (dropZeroBox || dropNoneHidden || dropZeroOpacity) {
        // Record the dropped element's selector so the transition
        // classifier can tell the difference between "DOM-new" (real
        // appearance) and "was filtered out, now visible" (fade-in).
        // Otherwise the appearance/fade label flips depending on
        // whether the caller passed `expected_targets`, which is a
        // knowledge-of-the-observer artefact, not a property of the
        // page.
        if (result.hidden_selectors.length < MAX_HIDDEN_SELECTORS) {
          result.hidden_selectors.push(makeSelector(el));
        }
        continue;
      }
    }
    const selector = makeSelector(el);
    const bbox = [r.x, r.y, r.width, r.height];
    // `getComputedStyle()` on a paused virtual clock does not always
    // reflect a @keyframes animation that the CDP Animation domain has
    // already evaluated — a hero starting at `opacity:0` reads as 0
    // every sample even though `getAnimations()[0].playState === 'finished'`
    // and the real opacity is 1. Reconcile by replaying the
    // element's own Web Animations: at `currentTime / activeDuration`
    // interpolate the opacity keyframes; for transform, interpolate
    // only the translate component (safe to extract via regex) and
    // fall through to the raw computed value when rotate / scale are
    // also present in the keyframe (those don't compose linearly).
    let effOpacity = parseFloat(cs.opacity);
    let effTransform = cs.transform === 'none' ? null : cs.transform;
    const runningAnimations = [];
    function extractTranslate(s) {
      // Extract (tx, ty) from supported CSS transform forms. Returns
      // {tx, ty, only_translate: bool} or null. `only_translate: true`
      // means the string contains nothing but a translate primitive
      // (so we can rebuild the result as a pure matrix without losing
      // information). When a rotate/scale is also present we still
      // record the translate but disable interpolation.
      if (typeof s !== 'string') return null;
      const t = s.trim();
      if (!t || t === 'none') return { tx: 0, ty: 0, only_translate: true };
      let m;
      if ((m = /^matrix\(\s*([-\d.eE]+)\s*,\s*([-\d.eE]+)\s*,\s*([-\d.eE]+)\s*,\s*([-\d.eE]+)\s*,\s*([-\d.eE]+)\s*,\s*([-\d.eE]+)\s*\)$/.exec(t))) {
        const a = +m[1], b = +m[2], c = +m[3], d = +m[4], tx = +m[5], ty = +m[6];
        // Only safe to interpolate if there's no rotation/scale (i.e.
        // the linear part is the identity).
        const isIdentityLinear = Math.abs(a - 1) < 1e-6 && Math.abs(d - 1) < 1e-6
          && Math.abs(b) < 1e-6 && Math.abs(c) < 1e-6;
        return { tx, ty, only_translate: isIdentityLinear };
      }
      if ((m = /^translate\(\s*([-\d.eE]+)px\s*(?:,\s*([-\d.eE]+)px)?\s*\)$/.exec(t))) {
        return { tx: +m[1], ty: m[2] !== undefined ? +m[2] : 0, only_translate: true };
      }
      if ((m = /^translateX\(\s*([-\d.eE]+)px\s*\)$/.exec(t))) {
        return { tx: +m[1], ty: 0, only_translate: true };
      }
      if ((m = /^translateY\(\s*([-\d.eE]+)px\s*\)$/.exec(t))) {
        return { tx: 0, ty: +m[1], only_translate: true };
      }
      if ((m = /^translate3d\(\s*([-\d.eE]+)px\s*,\s*([-\d.eE]+)px\s*,\s*[-\d.eE]+px\s*\)$/.exec(t))) {
        return { tx: +m[1], ty: +m[2], only_translate: true };
      }
      return null;
    }
    try {
      const anims = (el.getAnimations && el.getAnimations()) || [];
      for (const a of anims) {
        const ps = a.playState;
        if (ps !== 'running' && ps !== 'finished') continue;
        // Record the animation's name so the agent can cross-reference
        // `frame.active_sources[].name` against a hashed `cssId`. CSS
        // animations expose `animationName`; WAAPI / transitions use
        // `a.id` or fall back to the transition's property.
        const animName = a.animationName
          || (a.transitionProperty ? 'transition:' + a.transitionProperty : null)
          || (a.id ? a.id : null);
        if (animName && !runningAnimations.includes(animName)) {
          runningAnimations.push(animName);
        }
        const eff = a.effect;
        if (!eff) continue;
        const ct = (eff.getComputedTiming && eff.getComputedTiming()) || {};
        const dur = +ct.activeDuration || 0;
        const ctime = +a.currentTime || 0;
        const progress = ps === 'finished'
          ? 1
          : (dur > 0 ? Math.max(0, Math.min(1, ctime / dur)) : 0);
        const kfs = (eff.getKeyframes && eff.getKeyframes()) || [];
        // Opacity: interpolate from first/last keyframe's `opacity` if
        // present.
        const opKfs = kfs.filter(k => k.opacity !== undefined && k.opacity !== null);
        if (opKfs.length >= 2) {
          const a0 = parseFloat(opKfs[0].opacity);
          const a1 = parseFloat(opKfs[opKfs.length - 1].opacity);
          if (!Number.isNaN(a0) && !Number.isNaN(a1)) {
            effOpacity = a0 + (a1 - a0) * progress;
          }
        } else if (opKfs.length === 1) {
          const a0 = parseFloat(opKfs[0].opacity);
          if (!Number.isNaN(a0)) effOpacity = a0;
        }
        // Transform: interpolate the translate component when both
        // endpoints are pure translates (no rotate/scale to compose).
        // This is what lets `per_target_easing` resolve
        // `axis: transform-translate-y` on a CSS @keyframes that the
        // virtual clock would otherwise commit lazily — the same
        // mechanism opacity already uses, restricted to the safe
        // primitive.
        const trKfs = kfs.filter(k => k.transform !== undefined && k.transform !== null);
        if (trKfs.length >= 2) {
          const e0 = extractTranslate(trKfs[0].transform);
          const e1 = extractTranslate(trKfs[trKfs.length - 1].transform);
          if (e0 && e1 && e0.only_translate && e1.only_translate) {
            const tx = e0.tx + (e1.tx - e0.tx) * progress;
            const ty = e0.ty + (e1.ty - e0.ty) * progress;
            effTransform = 'matrix(1, 0, 0, 1, ' + tx + ', ' + ty + ')';
          }
        } else if (trKfs.length === 1) {
          const e0 = extractTranslate(trKfs[0].transform);
          if (e0 && e0.only_translate) {
            effTransform = 'matrix(1, 0, 0, 1, ' + e0.tx + ', ' + e0.ty + ')';
          }
        }
      }
    } catch (_) {}
    const entry = {
      selector: selector,
      tag: el.tagName.toLowerCase(),
      bbox: bbox,
      opacity: effOpacity,
      transform: effTransform,
      z_index: cs.zIndex === 'auto' ? null : cs.zIndex,
      overflow_x: cs.overflowX,
      overflow_y: cs.overflowY,
      display: cs.display,
      position: cs.position,
      color: cs.color || null,
      background_color: cs.backgroundColor || null,
      text_content_preview: null,
      matched_selectors: matchedSelectors(el, MATCH_SELECTORS),
      running_animations: runningAnimations,
      ancestor_match_selectors: ancestorMatchedSelectors(el, MATCH_SELECTORS),
    };
    // Only attach text content for leaf-ish elements (<= 1 element child).
    if (el.children.length <= 1) {
      const t = (el.textContent || '').trim();
      if (t) entry.text_content_preview = t.length > 80 ? t.slice(0, 80) + '...' : t;
    }
    visibles.push({ el, entry, cs, r });

    // Skip anomaly detection for explicitly ignored selectors.
    const ignoredForAnomalies = matchesAnySelector(el, IGNORE_SELECTORS);
    if (ignoredForAnomalies) continue;

    // -- Anomaly detection --
    if (r.right > vw + 1 && cs.position !== 'fixed') {
      pushAnomaly('horizontal-viewport-overflow', {
        selector: selector,
        bbox: bbox,
        detail: 'extends ' + Math.round(r.right - vw) + 'px past right viewport edge',
      });
    }
    const docScrollable = doc.scrollHeight > vh + 1;
    if (r.bottom > vh + 1 && !docScrollable && cs.position !== 'fixed') {
      pushAnomaly('vertical-viewport-overflow', {
        selector: selector,
        bbox: bbox,
        detail: 'extends ' + Math.round(r.bottom - vh) + 'px past bottom viewport edge in a non-scrollable document',
      });
    }
    const ox = cs.overflowX, oy = cs.overflowY;
    const scrollableX = ox === 'auto' || ox === 'scroll';
    const scrollableY = oy === 'auto' || oy === 'scroll';
    if (el.scrollWidth > el.clientWidth + 1 && ox !== 'visible' && !scrollableX) {
      pushAnomaly('content-clipped', {
        selector: selector,
        bbox: bbox,
        detail: 'scrollWidth=' + el.scrollWidth + ' > clientWidth=' + el.clientWidth + ' (overflow-x: ' + ox + ')',
      });
    }
    if (el.scrollHeight > el.clientHeight + 1 && oy !== 'visible' && !scrollableY) {
      pushAnomaly('content-clipped', {
        selector: selector,
        bbox: bbox,
        detail: 'scrollHeight=' + el.scrollHeight + ' > clientHeight=' + el.clientHeight + ' (overflow-y: ' + oy + ')',
      });
    }
    if (cs.textOverflow === 'ellipsis' && el.scrollWidth > el.clientWidth + 1) {
      pushAnomaly('text-ellipsis-truncated', {
        selector: selector,
        bbox: bbox,
        detail: 'ellipsis truncation active (scrollWidth=' + el.scrollWidth + ', clientWidth=' + el.clientWidth + ')',
      });
    }
    if (
      r.right <= 0 || r.bottom <= 0 || r.left >= vw || r.top >= vh
    ) {
      if (cs.position === 'absolute' || cs.position === 'fixed') {
        pushAnomaly('off-screen', {
          selector: selector,
          bbox: bbox,
          detail: 'positioned element is completely outside the viewport',
        });
      }
    }
  }

  // Apply optional selector filter to the elements list (anomalies are kept).
  // Elements satisfying a `match_selectors` entry are always retained even
  // when the focus filter would drop them, so `intent.expected_targets`
  // can still be graded.
  let displayed = visibles;
  if (SELECTOR_FILTERS && SELECTOR_FILTERS.length) {
    displayed = visibles.filter(v =>
      matchesAnySelector(v.el, SELECTOR_FILTERS) ||
      (MATCH_SELECTORS.length && matchesAnySelector(v.el, MATCH_SELECTORS)));
  }
  result.element_count = visibles.length;
  result.elements_returned = Math.min(displayed.length, MAX_ELEMENTS);
  result.truncated = displayed.length > MAX_ELEMENTS;
  result.elements = displayed.slice(0, MAX_ELEMENTS).map(v => v.entry);

  // Stacking overlap detection: positioned elements with similar bbox.
  const positioned = visibles.filter(v =>
    v.cs.position === 'absolute' || v.cs.position === 'fixed' || v.cs.position === 'sticky'
  );
  for (let i = 0; i < positioned.length; i++) {
    for (let j = i + 1; j < positioned.length; j++) {
      const a = positioned[i], b = positioned[j];
      if (a.el.contains(b.el) || b.el.contains(a.el)) continue;
      const ra = { x: a.r.x, y: a.r.y, w: a.r.width, h: a.r.height };
      const rb = { x: b.r.x, y: b.r.y, w: b.r.width, h: b.r.height };
      if (!rectsOverlap(ra, rb)) continue;
      // Significant overlap (> 50% of smaller area).
      const inter = Math.max(0, Math.min(ra.x + ra.w, rb.x + rb.w) - Math.max(ra.x, rb.x))
                  * Math.max(0, Math.min(ra.y + ra.h, rb.y + rb.h) - Math.max(ra.y, rb.y));
      const minArea = Math.min(ra.w * ra.h, rb.w * rb.h);
      if (minArea > 0 && inter / minArea > 0.5) {
        if (matchesAnySelector(a.el, IGNORE_SELECTORS) || matchesAnySelector(b.el, IGNORE_SELECTORS)) continue;
        pushAnomaly('overlap-stacking', {
          selector: a.entry.selector + ' <-> ' + b.entry.selector,
          bbox: [Math.min(ra.x, rb.x), Math.min(ra.y, rb.y), Math.max(ra.x + ra.w, rb.x + rb.w) - Math.min(ra.x, rb.x), Math.max(ra.y + ra.h, rb.y + rb.h) - Math.min(ra.y, rb.y)],
          detail: 'positioned elements overlap by more than 50% (z-index: ' + (a.cs.zIndex) + ' vs ' + (b.cs.zIndex) + ')',
        });
      }
    }
  }

  return JSON.stringify(result);
"#;

const PRELOAD_INSTRUMENTATION_JS: &str = r#"
(function () {
  if (window.__motionlens) { return; }
  const state = {
    rafCount: 0,
    rafActiveCount: 0,
    rafCancelCount: 0,
    setTimeoutCount: 0,
    setTimeoutActiveCount: 0,
    setTimeoutCancelCount: 0,
    setIntervalCount: 0,
    setIntervalActiveCount: 0,
    setIntervalCancelCount: 0,
    performanceNowCalls: 0,
    dateNowCalls: 0,
    // Bounded origin samples so we never leak unbounded memory on a busy page.
    rafOrigins: [],
    setTimeoutOrigins: [],
    setIntervalOrigins: [],
  };
  const ORIGIN_CAP = 50;
  window.__motionlens = state;

  function captureOrigin(bucket) {
    if (bucket.length >= ORIGIN_CAP) return;
    try {
      const lines = (new Error()).stack ? (new Error()).stack.split('\n') : [];
      // Drop the first three frames (Error, this hook, the user's call site
      // entrypoint) to make the recorded line point at user code.
      const head = lines.slice(3, 6).join(' | ');
      if (head) bucket.push(head);
    } catch (_) {}
  }

  try {
    const rafActive = new Set();
    const oraf = window.requestAnimationFrame;
    const ocaf = window.cancelAnimationFrame;
    if (oraf) {
      window.requestAnimationFrame = function (cb) {
        state.rafCount++;
        state.rafActiveCount++;
        captureOrigin(state.rafOrigins);
        const id = oraf.call(window, function (t) {
          if (rafActive.delete(id)) state.rafActiveCount--;
          return cb(t);
        });
        rafActive.add(id);
        return id;
      };
    }
    if (ocaf) {
      window.cancelAnimationFrame = function (id) {
        if (rafActive.delete(id)) {
          state.rafActiveCount--;
          state.rafCancelCount++;
        }
        return ocaf.call(window, id);
      };
    }

    const stActive = new Set();
    const ost = window.setTimeout;
    const oct = window.clearTimeout;
    window.setTimeout = function (cb, ms) {
      state.setTimeoutCount++;
      state.setTimeoutActiveCount++;
      captureOrigin(state.setTimeoutOrigins);
      const args = Array.prototype.slice.call(arguments, 2);
      const wrapped = (typeof cb === 'function')
        ? function () {
            if (stActive.delete(id)) state.setTimeoutActiveCount--;
            return cb.apply(this, args);
          }
        : cb;
      const id = ost.call(window, wrapped, ms);
      stActive.add(id);
      return id;
    };
    window.clearTimeout = function (id) {
      if (stActive.delete(id)) {
        state.setTimeoutActiveCount--;
        state.setTimeoutCancelCount++;
      }
      return oct.call(window, id);
    };

    const siActive = new Set();
    const osi = window.setInterval;
    const oci = window.clearInterval;
    window.setInterval = function () {
      state.setIntervalCount++;
      state.setIntervalActiveCount++;
      captureOrigin(state.setIntervalOrigins);
      const id = osi.apply(window, arguments);
      siActive.add(id);
      return id;
    };
    window.clearInterval = function (id) {
      if (siActive.delete(id)) {
        state.setIntervalActiveCount--;
        state.setIntervalCancelCount++;
      }
      return oci.call(window, id);
    };

    // Count `performance.now` / `Date.now` reads without changing their
    // values -- the virtual time policy already drives the returned numbers.
    // Use plain assignment (not defineProperty) because some Chromium builds
    // mark `performance.now` as non-configurable, which would throw and abort
    // the rest of the instrumentation.
    try {
      const opn = performance.now.bind(performance);
      performance.now = function () { state.performanceNowCalls++; return opn(); };
    } catch (_) {}
    try {
      const odn = Date.now;
      Date.now = function () { state.dateNowCalls++; return odn.call(Date); };
    } catch (_) {}

    // Long Animation Frame observer. Captures runtime jank as the
    // browser reports it (`long-animation-frame` entries are emitted
    // whenever the renderer's main thread blocks for more than ~50ms
    // during a frame). Under the paused virtual clock these entries
    // are rare (the renderer doesn't tick), but they DO fire during
    // `clock.advance` budget burn and during the paint flush that
    // follows a CDP-input trigger — exactly the windows where real
    // jank would surface. The entries are bounded so a misbehaving
    // page can't leak memory through this hook.
    state.loafEntries = [];
    const LOAF_CAP = 200;
    try {
      if (typeof PerformanceObserver === 'function' &&
          (PerformanceObserver.supportedEntryTypes || []).indexOf('long-animation-frame') !== -1) {
        const obs = new PerformanceObserver(function (list) {
          for (const e of list.getEntries()) {
            if (state.loafEntries.length >= LOAF_CAP) return;
            state.loafEntries.push({
              startTime: e.startTime,
              duration: e.duration,
              renderStart: e.renderStart || 0,
              styleAndLayoutStart: e.styleAndLayoutStart || 0,
              blockingDuration: e.blockingDuration || 0,
            });
          }
        });
        obs.observe({ type: 'long-animation-frame', buffered: true });
      }
    } catch (_) {}
  } catch (e) {
    // Instrumentation must never break the page.
  }
})();
"#;

pub struct Session {
    pub id: SessionId,
    browser: Option<Browser>,
    handler_task: Option<JoinHandle<()>>,
    animation_task: Option<JoinHandle<()>>,
    page: Page,
    driver: VirtualTimeDriver,
    collector: CdpTimelineCollector,
    artifact_store: ArtifactStore,
    active_episode: Option<EpisodeId>,
    episodes: Vec<EpisodeRecord>,
    launch_options: LaunchOptions,
    /// Per-session user-data-dir owned by this Session. Dropped (and the
    /// directory removed) when the Session is dropped, preventing
    /// `SingletonLock` collisions across concurrent runs.
    _user_data_dir: Option<tempfile::TempDir>,
}

/// Best-effort cleanup if `close()` was never awaited. We can't run async
/// browser shutdown from sync `Drop`, but we MUST abort the background tasks
/// (otherwise they outlive the Session) and trigger tempdir removal.
impl Drop for Session {
    fn drop(&mut self) {
        if let Some(task) = self.handler_task.take() {
            task.abort();
        }
        if let Some(task) = self.animation_task.take() {
            task.abort();
        }
        // Dropping the browser handle severs the CDP transport; the child
        // Chromium process will exit shortly thereafter on its own. The
        // tempdir's TempDir destructor takes care of removing the user-data
        // directory on disk.
        let _ = self.browser.take();
        let _ = self._user_data_dir.take();
    }
}

struct EpisodeRecord {
    id: EpisodeId,
    start_state: EpisodeStartState,
    ledger: EvidenceLedger,
}

impl Session {
    pub async fn launch(opts: LaunchOptions) -> Result<Self> {
        let session_id = SessionId::new();
        let artifact_store = ArtifactStore::for_session(&session_id)?;
        artifact_store.ensure_root().await?;

        let user_data_dir = tempfile::Builder::new()
            .prefix("ai-motionlens-")
            .tempdir()
            .map_err(|e| Error::browser(format!("create user_data_dir tempdir: {e}")))?;
        let mut builder = BrowserConfig::builder()
            .user_data_dir(user_data_dir.path())
            // Use the new headless mode (`--headless=new`). The
            // chromiumoxide 0.9 default (`HeadlessMode::True`) maps to
            // the legacy `--headless` flag which skips the full
            // rasterisation pipeline — Web fonts then fail to paint in
            // screenshots and `frame.capture` PNGs look half-empty
            // (`document.fonts.status: "loaded"` but the rendered
            // glyphs missing). The new headless mode is a proper
            // Chromium build with the same renderer as the visible
            // browser.
            .new_headless_mode()
            .viewport(Some(Viewport {
                width: opts.viewport_width,
                height: opts.viewport_height,
                device_scale_factor: Some(opts.device_scale_factor as f64),
                ..Default::default()
            }));
        if let Some(p) = resolve_chrome_path() {
            builder = builder.chrome_executable(p);
        }
        if !opts.headless {
            builder = builder.with_head();
        }
        let config = builder
            .build()
            .map_err(|e| Error::browser(format!("build BrowserConfig: {e}")))?;

        let (browser, mut handler) = Browser::launch(config).await.map_err(Error::browser)?;
        let handler_task =
            tokio::spawn(async move { while let Some(_event) = handler.next().await {} });

        let (page, animations, animation_task) = prepare_page(
            &browser,
            opts.url.as_str(),
            Some((
                opts.viewport_width,
                opts.viewport_height,
                opts.device_scale_factor,
            )),
            opts.external_state.as_ref(),
            opts.external_state_seed,
        )
        .await?;

        let driver = VirtualTimeDriver::new(page.clone(), 0.0);
        driver.initialize().await?;

        let collector = CdpTimelineCollector::new(page.clone(), true, animations);

        Ok(Self {
            id: session_id,
            browser: Some(browser),
            handler_task: Some(handler_task),
            animation_task: Some(animation_task),
            page,
            driver,
            collector,
            artifact_store,
            active_episode: None,
            episodes: Vec::new(),
            launch_options: opts,
            _user_data_dir: Some(user_data_dir),
        })
    }

    pub async fn close(&mut self) -> Result<()> {
        if let Some(mut browser) = self.browser.take() {
            if let Err(e) = browser.close().await {
                warn!("browser.close failed: {e}");
            }
            let _ = browser.wait().await;
        }
        if let Some(task) = self.handler_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.animation_task.take() {
            task.abort();
            let _ = task.await;
        }
        // Explicitly drop the tempdir so it is removed from disk as soon as
        // the agent calls `session.close`, rather than waiting for the outer
        // `Arc<Mutex<Session>>` to be released.
        let _ = self._user_data_dir.take();
        Ok(())
    }

    pub fn capabilities(&self) -> SessionCapabilities {
        let mut caps = self.driver.capabilities();
        if let Some(ref policy) = self.launch_options.external_state {
            caps.external_state = policy.clone();
        }
        caps
    }

    pub async fn clock_status(&self) -> Result<ClockStatus> {
        let mut status = self.driver.status().await?;
        status.current_episode_id = self.active_episode.clone();
        Ok(status)
    }

    /// Cheap one-shot probe of `document.title` + `location.href` from
    /// the live page. Used by `motion.verify` to stamp the report with
    /// what the page *actually* served, so a port collision / redirect /
    /// wrong build serving unexpected content at the same URL is visible
    /// at a glance instead of being mistaken for an animation defect
    /// (the contract URL alone is not enough — a dev port can hand you
    /// a different site).
    pub async fn observed_page_identity(&self) -> Result<(String, String)> {
        let js = "JSON.stringify({ t: document.title, u: location.href })";
        let raw = self.page.evaluate(js).await.map_err(Error::browser)?;
        let s = raw.into_value::<String>().unwrap_or_else(|_| "{}".into());
        let v: serde_json::Value = serde_json::from_str(&s).unwrap_or_default();
        let title = v
            .get("t")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let url = v
            .get("u")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        Ok((title, url))
    }

    // === episode ===

    /// Declare what the agent intends to build (Step 1 of the SKILL loop).
    /// Recorded into the active episode's ledger and consulted by
    /// `motion.assess` to flag mismatches.
    pub fn set_intent(&mut self, intent: EpisodeIntent) -> Result<()> {
        let ep_id = self
            .active_episode
            .clone()
            .ok_or_else(|| Error::NoActiveEpisode(self.id.to_string()))?;
        let ep = self
            .episodes
            .iter_mut()
            .find(|e| e.id == ep_id)
            .ok_or_else(|| Error::EpisodeNotFound(ep_id.to_string()))?;
        ep.ledger.intent = Some(intent);
        Ok(())
    }

    pub fn start_episode(&mut self) -> Result<(EpisodeId, EpisodeStartState)> {
        let started_at_wall_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        let start_state = EpisodeStartState {
            url: self.launch_options.url.clone(),
            viewport_width: self.launch_options.viewport_width,
            viewport_height: self.launch_options.viewport_height,
            device_scale_factor: self.launch_options.device_scale_factor,
            started_at_wall_ms,
            external_state: self
                .launch_options
                .external_state
                .clone()
                .unwrap_or_default(),
        };
        let id = EpisodeId::new();
        let ledger = EvidenceLedger {
            start_state: Some(start_state.clone()),
            ..Default::default()
        };
        self.episodes.push(EpisodeRecord {
            id: id.clone(),
            start_state: start_state.clone(),
            ledger,
        });
        self.active_episode = Some(id.clone());
        Ok((id, start_state))
    }

    pub async fn replay_to(&mut self, episode_id: &EpisodeId, t_ms: f64) -> Result<ReplayResult> {
        self.require_episode(episode_id)?;
        if t_ms < 0.0 {
            return Err(Error::invalid("t_ms must be >= 0"));
        }
        let (frame, result, _cost_ms) = self.scratch_replay_capture(episode_id, t_ms).await?;
        if let Some(ep) = self.episodes.iter_mut().find(|e| &e.id == episode_id) {
            ep.ledger.frames.push(frame);
        }
        Ok(result)
    }

    pub async fn clock_pause(&mut self) -> Result<()> {
        self.driver.pause().await
    }

    pub async fn clock_advance(
        &mut self,
        delta_ms: f64,
        max_starvation_count: Option<u32>,
    ) -> Result<AdvanceResult> {
        if delta_ms < 0.0 {
            return Err(Error::invalid("delta_ms must be >= 0"));
        }
        self.driver
            .advance(
                delta_ms,
                max_starvation_count.unwrap_or(DEFAULT_MAX_STARVATION_COUNT),
            )
            .await
    }

    // === triggers ===

    pub async fn trigger(
        &mut self,
        kind: TriggerKind,
        wait_policy: WaitPolicy,
        input_mode: crate::model::InputMode,
    ) -> Result<TriggerEventId> {
        let ep_id = self
            .active_episode
            .clone()
            .ok_or_else(|| Error::NoActiveEpisode(self.id.to_string()))?;

        let virtual_now = self.driver.status().await?.virtual_now_ms;
        let trigger_id = TriggerEventId::new();
        let event = TriggerEvent {
            trigger_id: trigger_id.clone(),
            at_t_ms: virtual_now,
            kind: kind.clone(),
            wait_policy,
            input_mode,
            wait_outcome: WaitOutcome::NotRequested,
        };

        if let Some(ep) = self.episodes.iter_mut().find(|e| e.id == ep_id) {
            ep.ledger.triggers.push(event);
        }

        // Dispatch synchronously at the current virtual time. Event listeners
        // run their synchronous body immediately; any rAF / transition / WAAPI
        // animation kicked off by the handler only advances when the agent
        // calls `clock.advance` or `frame.capture_series`. This keeps the
        // recorded `at_t_ms` honest and lets the agent observe the frame at
        // the exact moment of the trigger without an implicit budget burn.
        match input_mode {
            crate::model::InputMode::Js => {
                dispatch_trigger_on(&self.page, &kind).await?;
            }
            crate::model::InputMode::Cdp => {
                // Real compositor input. The dispatch returns immediately
                // but the input is queued and processed on the next paint;
                // under the paused virtual clock that paint won't happen
                // on its own, so we advance the clock by ~16ms (one
                // 60fps frame) right after to flush the pipeline.  That
                // budget burn is intentional and visible to the agent via
                // `clock.status` so the recorded `at_t_ms` for downstream
                // triggers stays honest.
                dispatch_trigger_on_cdp(&self.page, &kind).await?;
                self.driver.advance(16.0, 100).await?;
                self.driver.pause().await?;
            }
        }
        let outcome = honor_wait_policy(&self.page, wait_policy).await?;

        // Backfill the truthful wait outcome onto the recorded event so
        // anything reading the ledger (scratch replay, motion.assess,
        // motion.verify) can tell whether the wait actually held.
        if let Some(ep) = self.episodes.iter_mut().find(|e| e.id == ep_id) {
            if let Some(last) = ep.ledger.triggers.last_mut() {
                if last.trigger_id == trigger_id {
                    last.wait_outcome = outcome;
                }
            }
        }
        Ok(trigger_id)
    }

    // === frame capture ===

    pub async fn capture_frame(
        &mut self,
        format: ImageFormat,
        full_page: bool,
        layout_options: Option<&LayoutProbeOptions>,
        thumbnail: bool,
    ) -> Result<Frame> {
        let ep_id = self
            .active_episode
            .clone()
            .ok_or_else(|| Error::NoActiveEpisode(self.id.to_string()))?;

        let bytes = screenshot(&self.page, format, full_page).await?;
        let stored = self.artifact_store.save(&bytes, format, thumbnail).await?;
        let virtual_now = self.driver.status().await?.virtual_now_ms;

        let layout_snapshot = match layout_options {
            Some(opts) => Some(probe_layout(&self.page, opts).await?),
            None => None,
        };

        let active_sources = self.collector.active_sources_at(virtual_now).await;

        let frame = Frame {
            frame_id: FrameId::new(),
            episode_id: ep_id.clone(),
            t_ms: virtual_now,
            artifact_uri: stored.uri,
            artifact_local_path: stored.path.to_string_lossy().into_owned(),
            thumbnail_base64: stored.thumbnail_base64,
            width: stored.width,
            height: stored.height,
            format,
            active_sources,
            layout_snapshot,
        };

        if let Some(ep) = self.episodes.iter_mut().find(|e| e.id == ep_id) {
            ep.ledger.frames.push(frame.clone());
        }
        Ok(frame)
    }

    /// Run the layout probe at the current virtual time without saving an
    /// artifact image. Useful when the agent only wants the structural
    /// information (overflow / clipping / off-screen / stacking).
    pub async fn layout_probe(&self, options: &LayoutProbeOptions) -> Result<LayoutSnapshot> {
        probe_layout(&self.page, options).await
    }

    /// Read-only JS evaluation. Unlike `trigger.evaluate`, this is NOT recorded
    /// into the episode and is intended for queries (`getComputedStyle` /
    /// `getBoundingClientRect` / DOM inspection) where the agent wants a
    /// concrete return value rather than triggering a state change.
    pub async fn dom_query(&self, js: &str) -> Result<DomQueryResult> {
        let started = std::time::Instant::now();
        // Wrap user expression so we can return any JSON-serializable value,
        // including primitives and arrays. Errors during execution surface as
        // a Browser error.
        let wrapped = format!(
            "(function () {{ try {{ const __r = ({js}); return JSON.stringify({{ ok: true, v: __r === undefined ? null : __r }}); }} catch (e) {{ return JSON.stringify({{ ok: false, e: String(e && e.message || e) }}); }} }})()"
        );
        let result = self
            .page
            .evaluate(wrapped.as_str())
            .await
            .map_err(Error::browser)?;
        let s: String = result
            .into_value::<String>()
            .map_err(|e| Error::browser(format!("dom_query result not a string: {e}")))?;
        let parsed: serde_json::Value = serde_json::from_str(&s)
            .map_err(|e| Error::browser(format!("dom_query JSON parse: {e}")))?;
        if parsed.get("ok").and_then(|v| v.as_bool()) == Some(false) {
            let err = parsed
                .get("e")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            return Err(Error::invalid(format!("dom_query JS error: {err}")));
        }
        let value = parsed.get("v").cloned().unwrap_or(serde_json::Value::Null);
        Ok(DomQueryResult {
            value,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        })
    }

    pub async fn capture_series(
        &mut self,
        target_times_ms: &[f64],
        format: ImageFormat,
        full_page: bool,
        layout_options: Option<LayoutProbeOptions>,
        thumbnail: bool,
    ) -> Result<CaptureSeriesResult> {
        if target_times_ms.is_empty() {
            return Err(Error::invalid("target_times_ms must not be empty"));
        }
        let mut prev = f64::NEG_INFINITY;
        for t in target_times_ms {
            if *t < 0.0 {
                return Err(Error::invalid("each target time must be >= 0"));
            }
            if *t < prev {
                return Err(Error::invalid("target_times_ms must be non-decreasing"));
            }
            prev = *t;
        }

        let mut frames = Vec::new();
        let mut now = self.driver.status().await?.virtual_now_ms;
        for target in target_times_ms {
            let delta = target - now;
            if delta < 0.0 {
                return Err(Error::BackwardSeekUnsupported {
                    driver: "virtual-time",
                    target_ms: *target,
                    current_ms: now,
                });
            }
            if delta > 0.0 {
                let result = self.clock_advance(delta, None).await?;
                now = result.virtual_now_ms;
            }
            let f = self
                .capture_frame(format, full_page, layout_options.as_ref(), thumbnail)
                .await?;
            frames.push(f);
        }

        let sampled_points: Vec<f64> = target_times_ms.to_vec();
        let replay_cost = self.driver.capabilities().replay_cost;
        let mut intervals = Vec::with_capacity(target_times_ms.len().saturating_sub(1));
        for i in 1..target_times_ms.len() {
            let t0 = target_times_ms[i - 1];
            let t1 = target_times_ms[i];
            let span_ms = t1 - t0;
            let active_sources =
                union_active_sources(&frames[i - 1].active_sources, &frames[i].active_sources);
            let pixel_delta_hint = pixel_delta_hint_between(&frames[i - 1], &frames[i])
                .await
                .ok();
            let replay_cost_hint_ms = replay_cost.replay_fixed_cost_ms
                + replay_cost.cost_per_virtual_ms * span_ms
                + replay_cost.capture_cost_ms;
            intervals.push(UnobservedInterval {
                interval: Interval::new(t0, t1),
                span_ms,
                active_sources,
                pixel_delta_hint,
                recommended_next_t_ms: (t0 + t1) / 2.0,
                replay_cost_hint_ms,
            });
        }
        Ok(CaptureSeriesResult {
            frames,
            sampled_points,
            unobserved_intervals: intervals,
        })
    }

    pub async fn bisect(
        &mut self,
        episode_id: &EpisodeId,
        interval: Interval,
    ) -> Result<BisectResult> {
        self.require_episode(episode_id)?;
        if interval.span_ms() <= 0.0 {
            return Err(Error::invalid("interval must have positive span"));
        }
        let mid = interval.midpoint_ms();
        let live_now = self.driver.status().await?.virtual_now_ms;

        let (frame, replay_kind, cost_ms) = if mid > live_now {
            // Future relative to the live clock: advance + capture on live page.
            // `capture_frame` already appends to the active episode's ledger.
            self.clock_advance(mid - live_now, None).await?;
            let f = self
                .capture_frame(ImageFormat::Png, false, None::<&LayoutProbeOptions>, false)
                .await?;
            (f, BisectReplayKind::None, 0.0)
        } else {
            // Past: scratch-page replay. `scratch_replay_capture` does NOT
            // touch the ledger by itself (so its internal bisect-during-
            // replay flows stay side-effect-free) -- it's the caller's job
            // to record the captured frame in the originating episode.
            let (frame, _result, cost) = self.scratch_replay_capture(episode_id, mid).await?;
            if let Some(ep) = self.episodes.iter_mut().find(|e| &e.id == episode_id) {
                ep.ledger.frames.push(frame.clone());
            }
            (frame, BisectReplayKind::Scratch, cost)
        };

        // Hand the agent the same set of active sources we observed at the
        // bisection midpoint so they can reason about both sub-intervals
        // without re-querying.
        let mid_sources = frame.active_sources.clone();
        let left = UnobservedInterval {
            interval: Interval::new(interval.t0_ms, mid),
            span_ms: mid - interval.t0_ms,
            active_sources: mid_sources.clone(),
            pixel_delta_hint: None,
            recommended_next_t_ms: (interval.t0_ms + mid) / 2.0,
            replay_cost_hint_ms: cost_ms,
        };
        let right = UnobservedInterval {
            interval: Interval::new(mid, interval.t1_ms),
            span_ms: interval.t1_ms - mid,
            active_sources: mid_sources,
            pixel_delta_hint: None,
            recommended_next_t_ms: (mid + interval.t1_ms) / 2.0,
            replay_cost_hint_ms: cost_ms,
        };
        Ok(BisectResult {
            frame,
            remaining_intervals: vec![left, right],
            replay: replay_kind,
            replay_cost_ms: cost_ms,
        })
    }

    // === timeline detection ===

    pub async fn timeline_sources(&self) -> Result<TimelineSources> {
        self.collector.collect().await
    }

    /// Inspect the live page for time-dependent UI (CSS animations,
    /// transitions, WAAPI, rAF, scroll-driven, media) and decide whether a
    /// `motion.verify` evidence pass is required. The same `TimelineSources`
    /// that the caller would get from `timeline.sources` is returned so
    /// downstream tools can reuse it without a second probe.
    pub async fn audit_motion_sources(&self) -> Result<MotionAuditResult> {
        let sources = self.collector.collect().await?;
        let css_count = sources.css_animations.items.len()
            + sources.css_transitions.items.len()
            + sources.waapi.items.len();
        let scroll_count = sources.scroll_driven.items.len();
        let media_count = sources.media_elements.items.len();
        let raf_active = sources.raf.detected;
        let motion_detected = css_count > 0 || scroll_count > 0 || raf_active || media_count > 0;
        let reason = if !motion_detected {
            "No CSS animations / transitions / WAAPI / rAF / scroll-driven / media elements detected."
                .to_string()
        } else {
            let mut parts: Vec<String> = Vec::new();
            if css_count > 0 {
                parts.push(format!("{} CSS / WAAPI animations", css_count));
            }
            if scroll_count > 0 {
                parts.push(format!("{} scroll-driven animations", scroll_count));
            }
            if raf_active {
                parts.push(format!(
                    "rAF active ({} registrations)",
                    sources.raf.total_count
                ));
            }
            if media_count > 0 {
                parts.push(format!("{} <video>/<audio> elements", media_count));
            }
            format!("Motion detected: {}.", parts.join(", "))
        };
        // Recommended sampling: span 0..longest_duration at quartiles when a
        // duration is declared; otherwise sweep 0/200/500/1000/2000 ms.
        let longest = sources
            .css_animations
            .items
            .iter()
            .chain(sources.css_transitions.items.iter())
            .chain(sources.waapi.items.iter())
            .map(|a| a.delay_ms.max(0.0) + a.duration_ms.max(0.0))
            .fold(0.0_f64, f64::max);
        let recommended_target_times_ms = if longest > 0.0 {
            vec![0.0, longest * 0.25, longest * 0.5, longest * 0.75, longest]
        } else if motion_detected {
            vec![0.0, 200.0, 500.0, 1000.0, 2000.0]
        } else {
            Vec::new()
        };
        Ok(MotionAuditResult {
            motion_detected,
            sources,
            required_verification: motion_detected,
            recommended_target_times_ms,
            reason,
        })
    }

    /// On-disk root for this session's artifacts. Used by `motion.verify` to
    /// place `motionlens-report.json` next to the captured frames.
    pub fn artifact_store_root(&self) -> &std::path::Path {
        self.artifact_store.root_path()
    }

    /// Internal-only borrow of the live `Page` for paths that need to
    /// take an out-of-band screenshot (e.g. the final fullpage capture
    /// motion.verify offers). Outside crate boundaries the page is
    /// driven through high-level methods (`capture_frame`, `trigger`,
    /// etc.); this hook stays `pub(crate)`.
    pub(crate) fn page_ref(&self) -> &Page {
        &self.page
    }

    pub(crate) fn artifact_store_ref(&self) -> &ArtifactStore {
        &self.artifact_store
    }

    // === evidence ===

    pub fn evidence(&self, episode_id: &EpisodeId) -> Result<&EvidenceLedger> {
        self.episodes
            .iter()
            .find(|e| &e.id == episode_id)
            .map(|e| &e.ledger)
            .ok_or_else(|| Error::EpisodeNotFound(episode_id.to_string()))
    }

    pub fn append_observation(
        &mut self,
        episode_id: &EpisodeId,
        interval: Option<Interval>,
        claim: String,
        confidence: f64,
        evidence_frame_ids: Vec<FrameId>,
    ) -> Result<ObservationId> {
        let ep = self
            .episodes
            .iter_mut()
            .find(|e| &e.id == episode_id)
            .ok_or_else(|| Error::EpisodeNotFound(episode_id.to_string()))?;
        let id = ObservationId::new();
        ep.ledger.observations.push(Observation {
            observation_id: id.clone(),
            episode_id: episode_id.clone(),
            interval,
            claim,
            confidence,
            evidence_frame_ids,
        });
        Ok(id)
    }

    /// Compute a pixel-level diff between two previously captured frames.
    /// Reports `pixel_delta_ratio` only; DOM / style diffs are left empty
    /// pending a follow-up that snapshots layout state alongside the image.
    pub async fn motion_diff(
        &self,
        frame_id_a: &FrameId,
        frame_id_b: &FrameId,
    ) -> Result<MotionDiff> {
        let a = self
            .find_frame(frame_id_a)
            .ok_or_else(|| Error::FrameNotFound(frame_id_a.to_string()))?;
        let b = self
            .find_frame(frame_id_b)
            .ok_or_else(|| Error::FrameNotFound(frame_id_b.to_string()))?;
        let path_a = self
            .artifact_local_path(&a.artifact_uri)
            .ok_or_else(|| Error::invalid("artifact path for frame_a is not resolvable"))?;
        let path_b = self
            .artifact_local_path(&b.artifact_uri)
            .ok_or_else(|| Error::invalid("artifact path for frame_b is not resolvable"))?;
        let (bytes_a, bytes_b) =
            tokio::try_join!(tokio::fs::read(&path_a), tokio::fs::read(&path_b),)?;
        let pixel_delta_ratio = pixel_delta_ratio(&bytes_a, &bytes_b, a.format, b.format)?;

        let (bbox_changes, style_changes, appeared, disappeared) =
            layout_diff(a.layout_snapshot.as_ref(), b.layout_snapshot.as_ref());

        Ok(MotionDiff {
            frame_id_a: frame_id_a.clone(),
            frame_id_b: frame_id_b.clone(),
            pixel_delta_ratio,
            bbox_changes,
            style_changes,
            appeared,
            disappeared,
        })
    }

    /// Encode N captured frames as an animated GIF (or APNG) the user can
    /// view in any image viewer / share in chat. Frames are played at `fps`.
    pub async fn video_export(
        &mut self,
        frame_ids: &[FrameId],
        format: crate::model::VideoFormat,
        fps: f64,
    ) -> Result<crate::model::VideoExport> {
        if frame_ids.is_empty() {
            return Err(Error::invalid("video_export requires at least one frame"));
        }
        let fps = fps.clamp(1.0, 60.0);
        let delay_ms = (1000.0 / fps).round() as u16;

        use image::codecs::gif::{GifEncoder, Repeat};
        use image::{Frame as ImgFrame, ImageReader};
        use std::io::Cursor;

        // Decode frames into RGBA buffers.
        let mut decoded: Vec<image::RgbaImage> = Vec::with_capacity(frame_ids.len());
        let mut w: u32 = 0;
        let mut h: u32 = 0;
        for fid in frame_ids {
            let f = self
                .find_frame(fid)
                .ok_or_else(|| Error::FrameNotFound(fid.to_string()))?;
            let bytes = tokio::fs::read(&f.artifact_local_path).await?;
            let img_format = match f.format {
                ImageFormat::Png => image::ImageFormat::Png,
                ImageFormat::Jpeg => image::ImageFormat::Jpeg,
            };
            let img = ImageReader::with_format(Cursor::new(bytes), img_format)
                .decode()
                .map_err(|e| Error::invalid(format!("decode frame: {e}")))?
                .to_rgba8();
            if w == 0 {
                w = img.width();
                h = img.height();
            }
            decoded.push(img);
        }

        // Encode.
        let mut out_bytes: Vec<u8> = Vec::new();
        match format {
            crate::model::VideoFormat::Gif => {
                let mut enc = GifEncoder::new_with_speed(&mut out_bytes, 10);
                enc.set_repeat(Repeat::Infinite)
                    .map_err(|e| Error::invalid(format!("gif repeat: {e}")))?;
                for img in &decoded {
                    let delay = image::Delay::from_numer_denom_ms(delay_ms as u32, 1);
                    enc.encode_frame(ImgFrame::from_parts(img.clone(), 0, 0, delay))
                        .map_err(|e| Error::invalid(format!("gif encode frame: {e}")))?;
                }
                drop(enc);
            }
            crate::model::VideoFormat::Apng => {
                encode_apng(&decoded, w, h, delay_ms, &mut out_bytes)?;
            }
        }

        let stored = self.artifact_store.save_video(&out_bytes, format).await?;
        let frame_count = frame_ids.len() as u32;
        Ok(crate::model::VideoExport {
            artifact_uri: stored.uri,
            artifact_local_path: stored.path.to_string_lossy().into_owned(),
            format,
            width: w,
            height: h,
            frame_count,
            fps,
            duration_ms: (frame_count as f64) * (1000.0 / fps),
        })
    }

    /// Build a horizontal contact-sheet (time-strip) PNG from N captured
    /// frames. Saved as a new artifact and returned.
    ///
    /// `cell_metadata` (optional) supplies per-column `progress` / `scroll_y`
    /// values; these are burned onto the image and echoed in the response's
    /// `cells[]`. If omitted, labels show `t=Xms` only.
    pub async fn contact_sheet(
        &mut self,
        frame_ids: &[FrameId],
        thumb_width: u32,
        cell_metadata: Option<Vec<(Option<f64>, Option<f64>)>>,
    ) -> Result<ContactSheet> {
        if frame_ids.is_empty() {
            return Err(Error::invalid("contact_sheet requires at least one frame"));
        }
        let target_w = thumb_width.max(64);

        use image::{ImageBuffer, ImageReader, Rgba, RgbaImage};
        use std::io::Cursor;

        let mut thumbs: Vec<RgbaImage> = Vec::with_capacity(frame_ids.len());
        let mut frame_h: u32 = 0;
        let mut t_ms_list: Vec<f64> = Vec::with_capacity(frame_ids.len());
        // Per-thumb DOM-truth element rects (CSS-px bbox + the CSS viewport
        // width they were measured in) so the contact sheet can draw the
        // authoritative element positions on top of the screenshot. The
        // `layout_snapshot` is exact under the virtual clock; the
        // screenshot pixels can lag it by a compositor commit on sharp
        // near-instant keyframes (known platform limitation:
        // paused-clock screenshots and the compositor commit are not
        // tightly synchronised, see the bounded-timeout note below).
        // Overlaying the DOM rects makes the AI-facing visual correct
        // regardless of that pixel lag — the assessment math is untouched.
        let mut overlays: Vec<Option<(f64, Vec<[f64; 4]>)>> = Vec::with_capacity(frame_ids.len());
        for fid in frame_ids {
            let f = self
                .find_frame(fid)
                .ok_or_else(|| Error::FrameNotFound(fid.to_string()))?;
            t_ms_list.push(f.t_ms);
            let ov = f.layout_snapshot.as_ref().map(|ls| {
                let vw = if ls.viewport_width > 0.0 {
                    ls.viewport_width
                } else {
                    1280.0
                };
                let rects: Vec<[f64; 4]> = ls
                    .elements
                    .iter()
                    .filter(|e| e.bbox[2] > 1.0 && e.bbox[3] > 1.0)
                    .take(48)
                    .map(|e| e.bbox)
                    .collect();
                (vw, rects)
            });
            overlays.push(ov);
            let bytes = tokio::fs::read(&f.artifact_local_path).await?;
            let format = match f.format {
                ImageFormat::Png => image::ImageFormat::Png,
                ImageFormat::Jpeg => image::ImageFormat::Jpeg,
            };
            let img = ImageReader::with_format(Cursor::new(bytes), format)
                .decode()
                .map_err(|e| Error::invalid(format!("decode frame: {e}")))?;
            let ratio = target_w as f32 / img.width() as f32;
            let target_h = ((img.height() as f32) * ratio).round().max(1.0) as u32;
            let thumb = img.thumbnail_exact(target_w, target_h).to_rgba8();
            frame_h = frame_h.max(target_h);
            thumbs.push(thumb);
        }
        let gap: u32 = 4;
        let scale: u32 = 2; // font scale
        let label_h: u32 = (7 * scale) + 8; // glyph height + padding
        let sheet_w = target_w * thumbs.len() as u32 + gap * (thumbs.len() as u32 + 1);
        let sheet_h = frame_h + label_h + gap * 2;
        let mut sheet: RgbaImage =
            ImageBuffer::from_pixel(sheet_w, sheet_h, Rgba([20, 20, 24, 255]));
        let mut cells: Vec<ContactSheetCell> = Vec::with_capacity(frame_ids.len());
        for (i, thumb) in thumbs.iter().enumerate() {
            let x = gap + (target_w + gap) * i as u32;
            let y = gap;
            image::imageops::overlay(&mut sheet, thumb, x as i64, y as i64);

            // DOM-truth overlay: stroke the authoritative element rects
            // (exact under the virtual clock) so the AI-facing visual is
            // correct even when the screenshot pixels lag the compositor
            // on sharp keyframes (paused-clock screenshot vs commit
            // timing is a known platform limitation).
            if let Some((vw, rects)) = overlays.get(i).and_then(|o| o.as_ref()) {
                let s = target_w as f64 / vw.max(1.0);
                let th = thumb.height();
                let cx0 = x as i64;
                let cy0 = y as i64;
                let cx1 = cx0 + target_w as i64;
                let cy1 = cy0 + th as i64;
                let stroke = Rgba([60, 230, 120, 255]);
                for bb in rects {
                    let rx0 = cx0 + (bb[0] * s).round() as i64;
                    let ry0 = cy0 + (bb[1] * s).round() as i64;
                    let rx1 = cx0 + ((bb[0] + bb[2]) * s).round() as i64;
                    let ry1 = cy0 + ((bb[1] + bb[3]) * s).round() as i64;
                    let mut put = |px: i64, py: i64| {
                        if px >= cx0 && px < cx1 && py >= cy0 && py < cy1 {
                            sheet.put_pixel(px as u32, py as u32, stroke);
                        }
                    };
                    for t in 0..2i64 {
                        let mut px = rx0;
                        while px <= rx1 {
                            put(px, ry0 + t);
                            put(px, ry1 - t);
                            px += 1;
                        }
                        let mut py = ry0;
                        while py <= ry1 {
                            put(rx0 + t, py);
                            put(rx1 - t, py);
                            py += 1;
                        }
                    }
                }
            }

            let (progress, scroll_y) = cell_metadata
                .as_ref()
                .and_then(|m| m.get(i).copied())
                .unwrap_or((None, None));
            let t_ms = t_ms_list[i];
            let mut parts: Vec<String> = Vec::new();
            if let Some(p) = progress {
                parts.push(format!("p={:.2}", p));
            }
            parts.push(format!("t={:.0}ms", t_ms));
            if let Some(y) = scroll_y {
                parts.push(format!("y={:.0}", y));
            }
            let label = parts.join("  ");
            // Draw label onto the strip below the thumb.
            let (text_w, _) = crate::font::measure_text(&label, scale);
            let text_x = x as i32 + ((target_w as i32 - text_w as i32) / 2).max(0);
            let text_y = (gap + frame_h + 4) as i32;
            crate::font::draw_text(
                &mut sheet,
                &label,
                text_x,
                text_y,
                scale,
                [235, 235, 240, 255],
            );
            cells.push(ContactSheetCell {
                frame_id: frame_ids[i].clone(),
                column: i as u32,
                t_ms,
                progress,
                scroll_y,
                label,
            });
        }
        let mut buf: Vec<u8> = Vec::new();
        sheet
            .write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
            .map_err(|e| Error::invalid(format!("encode contact sheet: {e}")))?;
        let stored = self
            .artifact_store
            .save(&buf, ImageFormat::Png, false)
            .await?;
        Ok(ContactSheet {
            artifact_uri: stored.uri,
            artifact_local_path: stored.path.to_string_lossy().into_owned(),
            width: sheet_w,
            height: sheet_h,
            columns: thumbs.len() as u32,
            rows: 1,
            frame_ids: frame_ids.to_vec(),
            cells,
        })
    }

    /// Scroll-progress series. Advances the page scroll (instant, not
    /// smooth-scroll) to each requested progress value in [0..1], waits a
    /// small budget for scroll-driven animations to settle, and captures a
    /// frame at each step.
    pub async fn scroll_series(
        &mut self,
        progresses: &[f64],
        format: ImageFormat,
        layout_options: Option<LayoutProbeOptions>,
        thumbnail: bool,
        settle_ms: f64,
    ) -> Result<ScrollSeriesResult> {
        if progresses.is_empty() {
            return Err(Error::invalid("progresses must not be empty"));
        }
        for p in progresses {
            if !(0.0..=1.0).contains(p) {
                return Err(Error::invalid("progress values must be in [0.0, 1.0]"));
            }
        }

        // Read document scroll geometry up front.
        let geo = self
            .dom_query(
                "JSON.stringify({ sh: Math.max(document.documentElement.scrollHeight, document.body ? document.body.scrollHeight : 0), vh: window.innerHeight })",
            )
            .await?;
        let geo_s: String = serde_json::from_value(geo.value)
            .map_err(|e| Error::invalid(format!("scroll_series: read geo: {e}")))?;
        let geo_v: serde_json::Value = serde_json::from_str(&geo_s)
            .map_err(|e| Error::invalid(format!("scroll_series: parse geo: {e}")))?;
        let doc_sh = geo_v.get("sh").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let viewport_h = geo_v.get("vh").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let max_y = (doc_sh - viewport_h).max(0.0);

        let mut frames = Vec::with_capacity(progresses.len());
        let mut scroll_positions = Vec::with_capacity(progresses.len());
        for p in progresses {
            let target_y = max_y * p;
            let js = format!(
                "window.scrollTo({{ left: 0, top: {y}, behavior: 'instant' }})",
                y = target_y
            );
            self.page
                .evaluate(js.as_str())
                .await
                .map_err(Error::browser)?;
            if settle_ms > 0.0 {
                self.driver
                    .advance(settle_ms, DEFAULT_MAX_STARVATION_COUNT)
                    .await?;
            }
            let actual = self.dom_query("window.scrollY").await?;
            let actual_y = actual.value.as_f64().unwrap_or(target_y);
            scroll_positions.push(actual_y);
            let f = self
                .capture_frame(format, false, layout_options.as_ref(), thumbnail)
                .await?;
            frames.push(f);
        }
        Ok(ScrollSeriesResult {
            frames,
            scroll_positions,
            requested_progress: progresses.to_vec(),
            document_scroll_height: doc_sh,
            viewport_height: viewport_h,
        })
    }

    /// Translate `SectionProgressSpec` values (relative to a section's
    /// visibility in the viewport) into document-level scroll progress in
    /// `[0..1]`.
    pub async fn resolve_section_progress(&self, spec: &SectionProgressSpec) -> Result<Vec<f64>> {
        let sel_json = serde_json::to_string(&spec.selector).unwrap_or_else(|_| "\"\"".into());
        let js = format!(
            "(function(){{ const el = document.querySelector({sel}); if(!el) return null; const r = el.getBoundingClientRect(); const doc = document.documentElement; const scrollY = window.scrollY; const docH = Math.max(doc.scrollHeight, document.body ? document.body.scrollHeight : 0); const vh = window.innerHeight; return JSON.stringify({{ top: r.top + scrollY, h: r.height, doc_h: docH, vh: vh }}); }})()",
            sel = sel_json
        );
        let q = self.dom_query(&js).await?;
        let inner: String = match q.value {
            serde_json::Value::String(s) => s,
            serde_json::Value::Null => {
                return Err(Error::invalid(format!(
                    "selector not found: {}",
                    spec.selector
                )))
            }
            _ => return Err(Error::invalid("unexpected dom_query result shape")),
        };
        let v: serde_json::Value = serde_json::from_str(&inner)
            .map_err(|e| Error::invalid(format!("section probe parse: {e}")))?;
        let top = v.get("top").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let h = v.get("h").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let doc_h = v.get("doc_h").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let vh = v.get("vh").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let scrollable = (doc_h - vh).max(1.0);
        // Section visibility range: from when its top reaches viewport bottom
        // (just entering) to when its bottom leaves the viewport top (fully left).
        let start_y = (top - vh).max(0.0);
        let end_y = (top + h).min(scrollable);
        let span = (end_y - start_y).max(1.0);
        let mut out = Vec::with_capacity(spec.values.len());
        for raw in &spec.values {
            let v = raw.clamp(0.0, 1.0);
            let abs_y = start_y + v * span;
            let abs_progress = (abs_y / scrollable).clamp(0.0, 1.0);
            out.push(abs_progress);
        }
        Ok(out)
    }

    /// One-shot recipe for scroll-driven animations. Equivalent to:
    ///   scroll_series + contact_sheet + motion_assess
    /// in a single round-trip.
    ///
    /// `include` selects which parts to compute and return (default: all three).
    /// `include_frame_detail` controls whether the returned `series.frames[]`
    /// carries `thumbnail_base64` and `layout_snapshot` payloads. Default
    /// `false` to keep responses small; the agent should `Read` the
    /// `contact_sheet.artifact_local_path` for visual overview and the
    /// `assessment` for structured scores.
    pub async fn scroll_animation_check(
        &mut self,
        progresses: &[f64],
        target_selectors: Option<Vec<String>>,
        settle_ms: f64,
        include: &[RecipeInclude],
        include_frame_detail: bool,
    ) -> Result<ScrollAnimationCheckResult> {
        // The recipe is meant to be a one-shot high-level entry point —
        // an agent that just did `session.launch` and reached for
        // `recipes.scroll_animation_check` shouldn't have to know that
        // the underlying frame ledger needs an episode. Start one if
        // the caller hasn't.
        if self.active_episode.is_none() {
            self.start_episode()?;
        }
        let layout_opts = Some(LayoutProbeOptions {
            selectors: target_selectors,
            ignore_selectors: None,
            match_selectors: None,
            ignore_anomalies: None,
            max_elements: None,
        });
        // We always capture frames (the artifacts back contact_sheet + assess);
        // we just decide what to return to the caller and whether to strip
        // per-frame detail in the response.
        let series_full = self
            .scroll_series(progresses, ImageFormat::Png, layout_opts, false, settle_ms)
            .await?;
        let fids: Vec<FrameId> = series_full
            .frames
            .iter()
            .map(|f| f.frame_id.clone())
            .collect();

        let want_series = include.contains(&RecipeInclude::Series);
        let want_contact = include.contains(&RecipeInclude::ContactSheet);
        let want_assess = include.contains(&RecipeInclude::Assessment);

        let contact = if want_contact {
            let metadata: Vec<(Option<f64>, Option<f64>)> = (0..fids.len())
                .map(|i| {
                    (
                        series_full.requested_progress.get(i).copied(),
                        series_full.scroll_positions.get(i).copied(),
                    )
                })
                .collect();
            Some(self.contact_sheet(&fids, 320, Some(metadata)).await?)
        } else {
            None
        };
        let mut assess = if want_assess {
            Some(self.motion_assess(&fids).await?)
        } else {
            None
        };
        // Layer scroll-range match into the assessment intent_match for the
        // declared progress range, if any.
        if let Some(a) = assess.as_mut() {
            if let Some(im) = a.intent_match.as_mut() {
                let intent = self
                    .episodes
                    .iter()
                    .find(|e| e.id == a.episode_id)
                    .and_then(|ep| ep.ledger.intent.as_ref())
                    .cloned();
                if let Some(intent) = intent {
                    if let Some([start, end]) = intent.expected_progress_range {
                        let obs_start = *progresses.first().unwrap_or(&0.0);
                        let obs_end = *progresses.last().unwrap_or(&0.0);
                        let verdict = scroll_range_verdict(start, end, obs_start, obs_end);
                        im.scroll_range_match = Some(ScrollRangeMatch {
                            expected_start: start,
                            expected_end: end,
                            observed_start: obs_start,
                            observed_end: obs_end,
                            verdict: verdict.into(),
                        });
                        // intent_match.passes is NOT downgraded here.
                        // A `narrower-than-expected` verdict reflects a
                        // mismatch between the contract's
                        // `expected_progress_range` and the observed
                        // sample range (often a sticky / pin section
                        // whose ScrollTrigger active range differs from
                        // the section bbox progress) — that is a
                        // contract-vs-observation alignment issue, not a
                        // defect in the animation. It surfaces as an
                        // advisory `scroll-range-match` failed gate
                        // built downstream so the report still calls
                        // attention to it without flipping the
                        // correctness verdict.
                    }
                }
            }
        }

        let series_out = if want_series {
            let mut s = series_full;
            if !include_frame_detail {
                for f in &mut s.frames {
                    f.thumbnail_base64 = None;
                    f.layout_snapshot = None;
                }
            }
            Some(s)
        } else {
            None
        };
        // Build the recipe-level failed_gates list, mirroring how
        // `motion.verify` surfaces `scroll-range-match`. A non-match
        // verdict is advisory — it's a contract / observation
        // alignment issue (sticky pin window vs section bbox progress)
        // rather than an animation defect — so it shows up here but
        // does not flip `intent_match.passes`.
        use crate::model::FailedGate;
        use crate::model::GateReliability::Advisory;
        let mut failed_gates: Vec<FailedGate> = Vec::new();
        if let Some(srm) = assess
            .as_ref()
            .and_then(|a| a.intent_match.as_ref())
            .and_then(|im| im.scroll_range_match.as_ref())
        {
            if srm.verdict != "match" {
                failed_gates.push(FailedGate {
                    gate: "scroll-range-match".into(),
                    reliability: Advisory,
                    detail: format!(
                        "scroll_range_match.verdict = {:?} — expected \
                         scroll progress range [{:.2}..{:.2}] but \
                         observed [{:.2}..{:.2}]. This is a contract-\
                         vs-observation alignment issue, often from a \
                         `position: sticky` / pinned section whose \
                         ScrollTrigger active range differs from the \
                         section bbox progress the recipe samples \
                         against — narrow the recipe's sample range \
                         or loosen `expected_progress_range` to match \
                         the real pin/release window.",
                        srm.verdict,
                        srm.expected_start,
                        srm.expected_end,
                        srm.observed_start,
                        srm.observed_end
                    ),
                });
            }
        }
        Ok(ScrollAnimationCheckResult {
            series: series_out,
            contact_sheet: contact,
            assessment: assess,
            failed_gates,
        })
    }

    /// Run a structured quality assessment over an ordered sequence of frames.
    /// The frames must all belong to the same episode and be in ascending t_ms.
    pub async fn motion_assess(&self, frame_ids: &[FrameId]) -> Result<AnimationAssessment> {
        if frame_ids.len() < 2 {
            return Err(Error::invalid("motion.assess needs at least two frame_ids"));
        }
        let mut frames: Vec<&Frame> = Vec::with_capacity(frame_ids.len());
        for fid in frame_ids {
            let f = self
                .find_frame(fid)
                .ok_or_else(|| Error::FrameNotFound(fid.to_string()))?;
            frames.push(f);
        }
        let episode_id = frames[0].episode_id.clone();
        if !frames.iter().all(|f| f.episode_id == episode_id) {
            return Err(Error::invalid(
                "motion.assess: all frames must belong to the same episode",
            ));
        }
        for w in frames.windows(2) {
            if w[1].t_ms < w[0].t_ms {
                return Err(Error::invalid(
                    "motion.assess: frame_ids must be in ascending t_ms order",
                ));
            }
        }

        // Pairwise pixel deltas.
        let mut deltas: Vec<f64> = Vec::with_capacity(frames.len() - 1);
        for pair in frames.windows(2) {
            let a = pair[0];
            let b = pair[1];
            let path_a = self
                .artifact_local_path(&a.artifact_uri)
                .ok_or_else(|| Error::invalid("artifact path not resolvable"))?;
            let path_b = self
                .artifact_local_path(&b.artifact_uri)
                .ok_or_else(|| Error::invalid("artifact path not resolvable"))?;
            let (bytes_a, bytes_b) =
                tokio::try_join!(tokio::fs::read(&path_a), tokio::fs::read(&path_b),)?;
            let d = pixel_delta_ratio(&bytes_a, &bytes_b, a.format, b.format)?;
            deltas.push(d);
        }

        let n = deltas.len() as f64;
        let mean = deltas.iter().sum::<f64>() / n;
        let variance = deltas.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / n;
        let stddev = variance.sqrt();

        // Coefficient of variation. Smaller CV = more uniform per-frame change.
        // `smoothness = 1 / (1 + CV)` lands in (0, 1]. A static page has
        // CV = 0 which would otherwise be reported as `smoothness: 1.0` —
        // we flag that explicitly via `is_static` so callers never confuse
        // "no motion at all" with "perfectly smooth motion". This is a
        // first-pass verdict from the page-wide pixel-delta signal; the
        // DOM-truth observation (`per_target_easing` below) overrides
        // it when a small element moves on a large viewport and the
        // page-wide CV under-counts.
        let mut is_static = mean.abs() < NO_MOTION_FLOOR;
        let smoothness = if is_static {
            0.0
        } else {
            let cv = stddev / mean;
            1.0 / (1.0 + cv)
        };
        let smoothness_verdict = if is_static {
            "no-motion"
        } else if smoothness >= 0.8 {
            "good"
        } else if smoothness >= 0.6 {
            "acceptable"
        } else {
            "poor"
        }
        .to_string();

        // Jank detection: transitions whose delta exceeds mean + 2*stddev AND
        // whose absolute value is non-trivial (>0.01) to avoid noise on still
        // pages.
        let mut jank_events: Vec<JankEvent> = Vec::new();
        if stddev > 0.0 {
            for (i, d) in deltas.iter().enumerate() {
                let z = (d - mean) / stddev;
                if z >= 2.0 && *d > 0.01 {
                    jank_events.push(JankEvent {
                        from_t_ms: frames[i].t_ms,
                        to_t_ms: frames[i + 1].t_ms,
                        delta_ratio: *d,
                        z_score: z,
                        kind: JankKind::PixelCvOutlier,
                    });
                }
            }
        }
        // Positional-discontinuity jank: a whole-page pixel-CV outlier can
        // miss a small element that teleports (e.g. a 500px jump in one
        // interval) because the changed pixel ratio stays low. Detect it
        // structurally from the per-element bbox-center series so a sudden
        // jump is caught even under coarse sampling and even when the
        // pixel delta is uniform. Merge without duplicating an interval the
        // pixel pass already flagged.
        for pj in detect_positional_jank(&frames) {
            if !jank_events
                .iter()
                .any(|e| e.from_t_ms == pj.from_t_ms && e.to_t_ms == pj.to_t_ms)
            {
                jank_events.push(pj);
            }
        }
        jank_events.sort_by(|a, b| {
            a.from_t_ms
                .partial_cmp(&b.from_t_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let coverage_ms = frames.last().unwrap().t_ms - frames.first().unwrap().t_ms;

        // Classify per-transition motion kinds from layout snapshots.
        let per_transition_kinds: Vec<Vec<MotionCategory>> = frames
            .windows(2)
            .map(|pair| classify_transition(pair[0], pair[1]))
            .collect();
        let mut detected: Vec<MotionCategory> = Vec::new();
        for kinds in &per_transition_kinds {
            for k in kinds {
                if !detected.contains(k) {
                    detected.push(*k);
                }
            }
        }

        // Track which selectors actually triggered Appearance /
        // Disappearance so forbidden_kinds can be scoped to the
        // agent's `expected_targets`.
        let mut appeared_selectors: Vec<String> = Vec::new();
        let mut disappeared_selectors: Vec<String> = Vec::new();
        for pair in frames.windows(2) {
            let (ap, dis) = appeared_disappeared_between(pair[0], pair[1]);
            for s in ap {
                if !appeared_selectors.contains(&s) {
                    appeared_selectors.push(s);
                }
            }
            for s in dis {
                if !disappeared_selectors.contains(&s) {
                    disappeared_selectors.push(s);
                }
            }
        }

        // Selectors that actually moved across any adjacent pair. Required
        // to evaluate `intent.expected_targets`.
        let mut moved_selectors: Vec<String> = Vec::new();
        for pair in frames.windows(2) {
            for sel in moved_selectors_between(pair[0], pair[1]) {
                if !moved_selectors.contains(&sel) {
                    moved_selectors.push(sel);
                }
            }
        }

        // Per-selector easing fit + overshoot + stagger uniformity.
        // Computed *before* `intent_match` so we can reconcile
        // `detected_motion_kinds` (per-frame-pair classifier) with
        // `per_target_easing` (sequence min/max amplitude). On a CSS
        // `transition` that completes in ~80ms with samples at
        // 0/40/80/160/240ms, the per-pair classifier can find every
        // adjacent delta under its 0.5px threshold (e.g.
        // [-9.35, -9.74, ..., -10]) and emit no `translate` kind, even
        // though the sequence min/max range is 10px and
        // per_target_easing reports `axis: transform-translate-y`.
        // Treating that as a `detected` translate keeps the two
        // pipelines in agreement so a hover contract declaring
        // `expected_kinds: ["translate"]` doesn't get
        // `expected_kinds_missing` while the easing fit shows the same
        // axis.
        let (per_target_easing, overshoot_events, stagger_uniformity) =
            compute_easing_stagger_overshoot(&frames, &moved_selectors);
        for te in &per_target_easing {
            let kind = match te.axis.as_str() {
                "transform-translate-x"
                | "transform-translate-y"
                | "translate-x"
                | "translate-y" => Some(MotionCategory::Translate),
                "opacity" => Some(MotionCategory::Fade),
                _ => None,
            };
            if let Some(k) = kind {
                if !detected.contains(&k) {
                    detected.push(k);
                }
            }
        }
        // Reconcile is_static against the DOM-truth observation. The
        // page-wide pixel-delta `mean` is a fraction of the whole
        // viewport, so a hero word stagger that translates 100px on a
        // 1280x800 page produces a mean ~2e-5 (well under
        // NO_MOTION_FLOOR) — yet `per_target_easing` clearly fits
        // `transform-translate-y` with 100px amplitude. The headline
        // `is_static` should reflect that something moved, even when
        // the pixel-CV under-counts. `smoothness` / `smoothness_verdict`
        // are kept as-is: they specifically describe the page-wide CV
        // signal and remain accurate as a "the page as a whole isn't
        // moving much" advisory. Without this reconcile the `non-static`
        // correctness gate fires on every small-bbox-on-large-page
        // motion that intent_match has already graded as passing (the
        // canonical case is a hero-word stagger).
        if is_static && !per_target_easing.is_empty() {
            is_static = false;
        }

        // Intent diff.
        let intent_clone = self
            .episodes
            .iter()
            .find(|e| e.id == episode_id)
            .and_then(|ep| ep.ledger.intent.clone());
        let mut intent_match = intent_clone.as_ref().map(|intent| {
            build_intent_match(
                intent,
                &detected,
                &moved_selectors,
                coverage_ms,
                &per_target_easing,
                &appeared_selectors,
                &disappeared_selectors,
            )
        });

        let mut recommendations: Vec<String> = Vec::new();
        if mean.abs() < NO_MOTION_FLOOR {
            recommendations.push(
                "Per-frame pixel delta is essentially zero across the sequence; nothing animated. \
                 Check whether the animation is registered (timeline.sources), whether the clock \
                 actually advanced (clock.status), or whether animation-play-state is 'paused'."
                    .into(),
            );
        }
        if smoothness < 0.6 && !jank_events.is_empty() {
            recommendations.push(
                "Smoothness is low and discrete jank events were detected. Consider denser \
                 sampling around each jank event with frame.bisect, then decide whether the spike \
                 is an intended beat (modal appearing, layout shift) or an unintended jump."
                    .into(),
            );
        }
        if smoothness > 0.85 && jank_events.is_empty() && mean > NO_MOTION_FLOOR {
            recommendations.push(
                "Animation is smooth and contains no detected jank under this sampling. If the \
                 perceived quality is still off, look at timing (timeline.sources duration vs your \
                 sample span) or easing fidelity (sample more densely around the start/end)."
                    .into(),
            );
        }

        let summary = format!(
            "Analyzed {} frames over {:.1}ms. Mean per-transition pixel delta: {:.4} (stddev {:.4}). \
             Smoothness: {:.2}/1.00 — relative WITHIN this sample_plan, not an absolute quality \
             score: denser/uneven sampling lowers it even on a 60fps-smooth animation, so read it \
             alongside the contact sheet and runtime_jank_events, not on its own. Jank events: {}.",
            frames.len(),
            coverage_ms,
            mean,
            stddev,
            smoothness,
            jank_events.len()
        );

        if let Some(im) = &intent_match {
            if !im.passes {
                if !im.expected_kinds_missing.is_empty() {
                    recommendations.push(format!(
                        "Intent declared expected_kinds {:?} but they were never observed. Either the animation does not implement them or the sampling missed them.",
                        im.expected_kinds_missing
                    ));
                }
                if !im.forbidden_kinds_seen.is_empty() {
                    recommendations.push(format!(
                        "Forbidden motion kinds were observed: {:?}. Fix the code to remove them.",
                        im.forbidden_kinds_seen
                    ));
                }
                if !im.expected_targets_missing.is_empty() {
                    recommendations.push(format!(
                        "Intent declared expected_targets {:?} but they never moved (no bbox / opacity / transform change in any adjacent frame pair). Verify the selectors match the actual elements, or sample more densely.",
                        im.expected_targets_missing
                    ));
                }
                if let Some(d) = &im.duration_match {
                    if d.verdict != "match" {
                        recommendations.push(format!(
                            "Duration mismatch: expected {:.0}ms, observed coverage {:.0}ms (ratio {:.2} / {}).",
                            d.expected_ms, d.observed_coverage_ms, d.ratio, d.verdict
                        ));
                    }
                }
            }
        }

        // Runtime jank — drained from the page's LoAF observer.  Best
        // effort: when the API is missing or no entries fired this
        // returns an empty vec.
        let runtime_jank_events = collect_runtime_jank(&self.page).await.unwrap_or_default();

        // Fold easing_match into intent_match when intent.expected_easing
        // was declared. Majority-vote over per_target_easing decides the
        // observed easing template.
        if let (Some(intent), Some(im)) = (intent_clone.as_ref(), intent_match.as_mut()) {
            if let Some(expected) = intent.expected_easing {
                let mut counts: std::collections::HashMap<&str, u32> =
                    std::collections::HashMap::new();
                for te in &per_target_easing {
                    *counts.entry(te.best_match_easing.as_str()).or_insert(0) += 1;
                }
                let observed = counts
                    .into_iter()
                    .max_by_key(|(_, c)| *c)
                    .map(|(name, _)| name.to_string());
                let expected_label = easing_hint_label(expected).to_string();
                let easing_passes = match observed.as_deref() {
                    Some(obs) => obs == expected_label,
                    None => false,
                };
                im.easing_match = Some(crate::model::EasingMatch {
                    expected: expected_label,
                    observed: observed.unwrap_or_else(|| "unknown".into()),
                    passes: easing_passes,
                });
                if !easing_passes {
                    im.passes = false;
                }
            }
        }

        Ok(AnimationAssessment {
            episode_id,
            analyzed_frame_ids: frame_ids.to_vec(),
            frame_count: frames.len() as u32,
            coverage_ms,
            per_transition_delta: deltas,
            mean_delta: mean,
            stddev_delta: stddev,
            smoothness,
            smoothness_verdict,
            is_static,
            jank_events,
            detected_motion_kinds: detected,
            moved_selectors,
            appeared_selectors,
            disappeared_selectors,
            per_transition_kinds,
            per_target_easing,
            stagger_uniformity,
            overshoot_events,
            runtime_jank_events,
            intent_match,
            summary,
            recommendations,
        })
    }

    fn find_frame(&self, frame_id: &FrameId) -> Option<&Frame> {
        for ep in &self.episodes {
            if let Some(f) = ep.ledger.frames.iter().find(|f| &f.frame_id == frame_id) {
                return Some(f);
            }
        }
        None
    }

    pub fn artifact_local_path(&self, artifact_uri: &str) -> Option<std::path::PathBuf> {
        let prefix = format!(
            "motionlens://artifacts/{}/",
            self.artifact_store
                .root_path()
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("session")
        );
        artifact_uri
            .strip_prefix(&prefix)
            .map(|tail| self.artifact_store.root_path().join(tail))
    }

    // === scratch replay ===

    /// Open a new scratch page, re-establish the episode start state, replay
    /// every recorded trigger in order, advance the scratch clock to
    /// `target_t_ms`, capture there, and close the scratch page. The live page
    /// is not touched.
    async fn scratch_replay_capture(
        &mut self,
        episode_id: &EpisodeId,
        target_t_ms: f64,
    ) -> Result<(Frame, ReplayResult, f64)> {
        let started_at = Instant::now();

        // Pull what we need from the episode without holding a borrow across awaits.
        let (triggers, start_state) = {
            let ep = self
                .episodes
                .iter()
                .find(|e| &e.id == episode_id)
                .ok_or_else(|| Error::EpisodeNotFound(episode_id.to_string()))?;
            (ep.ledger.triggers.clone(), ep.start_state.clone())
        };

        let browser = self
            .browser
            .as_ref()
            .ok_or_else(|| Error::invalid("browser is closed"))?;

        let (scratch_page, scratch_animations, scratch_task) = prepare_page(
            browser,
            start_state.url.as_str(),
            Some((
                start_state.viewport_width,
                start_state.viewport_height,
                start_state.device_scale_factor,
            )),
            self.launch_options.external_state.as_ref(),
            self.launch_options.external_state_seed,
        )
        .await?;

        let scratch_driver = VirtualTimeDriver::new(scratch_page.clone(), 0.0);
        scratch_driver.initialize().await?;
        let scratch_collector =
            CdpTimelineCollector::new(scratch_page.clone(), true, scratch_animations);

        let mut virtual_now = 0.0_f64;
        let mut replayed_triggers: u32 = 0;
        let mut driver = scratch_driver;

        for ev in &triggers {
            if ev.at_t_ms > target_t_ms {
                break;
            }
            if ev.at_t_ms > virtual_now {
                let delta = ev.at_t_ms - virtual_now;
                driver.advance(delta, DEFAULT_MAX_STARVATION_COUNT).await?;
                virtual_now = ev.at_t_ms;
            }
            match ev.input_mode {
                InputMode::Cdp => {
                    dispatch_trigger_on_cdp(&scratch_page, &ev.kind).await?;
                }
                InputMode::Js => {
                    dispatch_trigger_on(&scratch_page, &ev.kind).await?;
                }
            }
            honor_wait_policy(&scratch_page, ev.wait_policy).await?;
            replayed_triggers += 1;
        }

        if virtual_now < target_t_ms {
            let delta = target_t_ms - virtual_now;
            driver.advance(delta, DEFAULT_MAX_STARVATION_COUNT).await?;
            virtual_now = target_t_ms;
        }

        let bytes = screenshot(&scratch_page, ImageFormat::Png, false).await?;
        let stored = self
            .artifact_store
            .save(&bytes, ImageFormat::Png, false)
            .await?;
        // Match live `capture_frame`: always include a default layout snapshot
        // so callers (bisect / replay_to) can hand the frame off to
        // `motion.diff` / `motion.assess` without losing structured signal.
        let layout_snapshot = probe_layout(&scratch_page, &LayoutProbeOptions::default())
            .await
            .ok();
        let active_sources = scratch_collector.active_sources_at(virtual_now).await;
        let frame = Frame {
            frame_id: FrameId::new(),
            episode_id: episode_id.clone(),
            t_ms: virtual_now,
            artifact_uri: stored.uri,
            artifact_local_path: stored.path.to_string_lossy().into_owned(),
            thumbnail_base64: stored.thumbnail_base64,
            width: stored.width,
            height: stored.height,
            format: ImageFormat::Png,
            active_sources,
            layout_snapshot,
        };

        scratch_task.abort();
        let _ = scratch_page.close().await;

        let cost_ms = started_at.elapsed().as_secs_f64() * 1000.0;
        let result = ReplayResult {
            virtual_now_ms: virtual_now,
            replayed_triggers,
            outcome: AdvanceOutcome::Advanced,
            frame_id: Some(frame.frame_id.clone()),
        };
        Ok((frame, result, cost_ms))
    }

    // === helpers ===

    fn require_episode(&self, episode_id: &EpisodeId) -> Result<()> {
        if self.episodes.iter().any(|e| &e.id == episode_id) {
            Ok(())
        } else {
            Err(Error::EpisodeNotFound(episode_id.to_string()))
        }
    }
}

// === free helpers shared between live + scratch flows ===

async fn prepare_page(
    browser: &Browser,
    url: &str,
    viewport: Option<(u32, u32, f32)>,
    external_state: Option<&ExternalStatePolicy>,
    external_state_seed: u64,
) -> Result<(Page, AnimationBuffer, JoinHandle<()>)> {
    use crate::model::ExternalSourceMode;
    let page = browser
        .new_page("about:blank")
        .await
        .map_err(Error::browser)?;

    // Mirror the original launch viewport / DSF onto this page. The browser-
    // level config already covers the live page, but `new_page` for scratch
    // replays needs to pin viewport explicitly so a replay does not silently
    // measure under a different layout.
    if let Some((w, h, dsf)) = viewport {
        use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
        let params = SetDeviceMetricsOverrideParams::builder()
            .width(w as i64)
            .height(h as i64)
            .device_scale_factor(dsf as f64)
            .mobile(false)
            .build()
            .map_err(|e| Error::browser(format!("build SetDeviceMetricsOverride: {e}")))?;
        page.execute(params).await.map_err(Error::browser)?;
    }

    // Apply external-state overrides BEFORE the instrumentation preload
    // so the seeded RNG hook is in place when the page's own scripts
    // run on first navigation.
    if let Some(policy) = external_state {
        if matches!(policy.random, ExternalSourceMode::Pinned)
            || matches!(policy.crypto, ExternalSourceMode::Pinned)
        {
            let preload = build_seeded_rng_preload(
                external_state_seed,
                policy.crypto == ExternalSourceMode::Pinned,
            );
            page.evaluate_on_new_document(preload.as_str())
                .await
                .map_err(Error::browser)?;
        }
        if matches!(policy.timezone, ExternalSourceMode::Pinned) {
            use chromiumoxide::cdp::browser_protocol::emulation::SetTimezoneOverrideParams;
            let p = SetTimezoneOverrideParams::builder()
                .timezone_id("UTC")
                .build()
                .map_err(|e| Error::browser(format!("build SetTimezoneOverride: {e}")))?;
            page.execute(p).await.map_err(Error::browser)?;
        }
        if matches!(policy.locale, ExternalSourceMode::Pinned) {
            use chromiumoxide::cdp::browser_protocol::emulation::SetLocaleOverrideParams;
            let p = SetLocaleOverrideParams::builder().locale("en-US").build();
            page.execute(p).await.map_err(Error::browser)?;
        }
        if matches!(policy.user_agent, ExternalSourceMode::Pinned) {
            use chromiumoxide::cdp::browser_protocol::emulation::SetUserAgentOverrideParams;
            let p = SetUserAgentOverrideParams::builder()
                .user_agent(
                    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
                     AppleWebKit/537.36 (KHTML, like Gecko) \
                     ai-motionlens/0.1 deterministic",
                )
                .build()
                .map_err(|e| Error::browser(format!("build SetUserAgentOverride: {e}")))?;
            page.execute(p).await.map_err(Error::browser)?;
        }
    }

    page.evaluate_on_new_document(PRELOAD_INSTRUMENTATION_JS)
        .await
        .map_err(Error::browser)?;

    page.execute(AnimationEnableParams::default())
        .await
        .map_err(Error::browser)?;

    let animations: AnimationBuffer = Arc::new(Mutex::new(Vec::new()));
    let task = {
        let buf = animations.clone();
        let mut stream = page
            .event_listener::<EventAnimationStarted>()
            .await
            .map_err(Error::browser)?;
        tokio::spawn(async move {
            while let Some(ev) = stream.next().await {
                buf.lock().await.push(ev.animation.clone());
            }
        })
    };

    page.goto(url).await.map_err(Error::browser)?;
    page.wait_for_navigation().await.map_err(Error::browser)?;
    // Freeze the virtual clock the instant navigation commits — before any
    // further wall-clock elapses. Any real time that passes between commit
    // and pause leaks into `document.timeline` and offsets every
    // subsequently-armed animation's start. Pausing here — with the
    // renderer already alive post-navigation — is safe (unlike a pre-`goto`
    // pause, which freezes the renderer and deadlocks navigation) and cuts
    // the leak to the irreducible navigation-commit time.
    // `driver.initialize()` re-asserts this pause idempotently and
    // additionally enables lifecycle events; the storage pin below runs
    // under pause (a one-shot DOM op, not time-dependent).
    {
        use chromiumoxide::cdp::browser_protocol::emulation::{
            SetVirtualTimePolicyParams, VirtualTimePolicy,
        };
        let pause = SetVirtualTimePolicyParams::builder()
            .policy(VirtualTimePolicy::Pause)
            .build()
            .map_err(|e| {
                Error::browser(format!("build SetVirtualTimePolicy(Pause) post-nav: {e}"))
            })?;
        page.execute(pause).await.map_err(Error::browser)?;
    }

    // Storage pin runs AFTER the initial navigation completes — we want
    // the page's own load-time storage writes to be discarded, leaving
    // each replay starting from a known empty state.  Cookies are cleared
    // at the origin level; localStorage / sessionStorage are cleared via
    // JS because the CDP storage domain requires elevated permissions to
    // wipe origin storage on every Chromium build.
    if let Some(policy) = external_state {
        if matches!(policy.storage, ExternalSourceMode::Pinned) {
            let _ = page
                .evaluate(
                    "(function(){try{localStorage.clear();}catch(e){} try{sessionStorage.clear();}catch(e){} \
                      try{document.cookie.split(';').forEach(function(c){const eq=c.indexOf('='); const name=eq>-1?c.slice(0,eq).trim():c.trim(); document.cookie=name+'=; expires=Thu, 01 Jan 1970 00:00:00 GMT; path=/';});}catch(e){}})()",
                )
                .await;
        }
    }

    Ok((page, animations, task))
}

/// Build a small preload script that swaps `Math.random` and (optionally)
/// `crypto.getRandomValues` for a seeded xorshift PRNG.  The seed lives
/// on `window.__motionlens_rng` so the agent can re-read it after
/// navigation (`frame.dom_query { js: "window.__motionlens_rng.seed" }`)
/// to confirm determinism was actually wired up.
fn build_seeded_rng_preload(seed: u64, pin_crypto: bool) -> String {
    // Take the low 32 bits of the seed for the xorshift state (Math.random
    // returns 32-bit precision anyway). Zero is a degenerate xorshift state,
    // so we substitute a canonical non-zero value.
    let seed32 = ((seed as u32) ^ ((seed >> 32) as u32)).max(1);
    format!(
        "(function () {{\n\
           if (window.__motionlens_rng) return;\n\
           let s = {seed} >>> 0;\n\
           const rng = function () {{ s ^= s << 13; s >>>= 0; s ^= s >>> 17; s ^= s << 5; s >>>= 0; return (s & 0xfffffff) / 0xfffffff; }};\n\
           window.__motionlens_rng = {{ seed: {seed}, next: rng }};\n\
           try {{ Math.random = rng; }} catch (_) {{}}\n\
           if ({pin_crypto}) {{\n\
             try {{\n\
               const c = window.crypto;\n\
               if (c) {{\n\
                 c.getRandomValues = function (buf) {{\n\
                   for (let i = 0; i < buf.length; i++) {{\n\
                     const r = rng();\n\
                     if (buf instanceof Float32Array || buf instanceof Float64Array) {{ buf[i] = r; }}\n\
                     else if (buf instanceof BigUint64Array || buf instanceof BigInt64Array) {{ buf[i] = BigInt(Math.floor(r * Number.MAX_SAFE_INTEGER)); }}\n\
                     else {{ buf[i] = Math.floor(r * 0x100000000) & 0xffffffff; }}\n\
                   }}\n\
                   return buf;\n\
                 }};\n\
               }}\n\
             }} catch (_) {{}}\n\
           }}\n\
         }})();",
        seed = seed32,
        pin_crypto = if pin_crypto { "true" } else { "false" },
    )
}

async fn probe_layout(page: &Page, options: &LayoutProbeOptions) -> Result<LayoutSnapshot> {
    let opts_json = serde_json::json!({
        "selectors": options.selectors,
        "ignore_selectors": options.ignore_selectors,
        "match_selectors": options.match_selectors,
        "ignore_anomalies": options.ignore_anomalies.as_ref().map(|v| {
            v.iter()
                .map(|k| serde_json::to_value(k).unwrap_or(serde_json::Value::Null))
                .collect::<Vec<_>>()
        }),
        "max_elements": options.max_elements.unwrap_or(LAYOUT_PROBE_MAX_ELEMENTS as u32),
    });
    let js = format!(
        "(function(opts){{ return (function(){{ {body} }})(opts); }})({opts});",
        body = LAYOUT_PROBE_JS_BODY,
        opts = opts_json
    );
    let result = page.evaluate(js.as_str()).await.map_err(Error::browser)?;
    let json: String = result
        .into_value::<String>()
        .map_err(|e| Error::browser(format!("layout probe result not a string: {e}")))?;
    let snapshot: LayoutSnapshot = serde_json::from_str(&json)
        .map_err(|e| Error::browser(format!("layout probe JSON parse: {e}")))?;
    Ok(snapshot)
}

/// Compare two layout snapshots and produce structured deltas. The probe's
/// selector strings are stable enough for one episode's worth of frames to
/// match across captures.
fn layout_diff(
    a: Option<&LayoutSnapshot>,
    b: Option<&LayoutSnapshot>,
) -> (Vec<BboxChange>, Vec<StyleChange>, Vec<String>, Vec<String>) {
    let (a, b) = match (a, b) {
        (Some(a), Some(b)) => (a, b),
        _ => return (Vec::new(), Vec::new(), Vec::new(), Vec::new()),
    };

    use std::collections::HashMap;
    let map_a: HashMap<&str, &crate::model::ElementProbe> = a
        .elements
        .iter()
        .map(|e| (e.selector.as_str(), e))
        .collect();
    let map_b: HashMap<&str, &crate::model::ElementProbe> = b
        .elements
        .iter()
        .map(|e| (e.selector.as_str(), e))
        .collect();

    let mut bbox_changes = Vec::new();
    let mut style_changes = Vec::new();
    let mut appeared = Vec::new();
    let mut disappeared = Vec::new();

    for (sel, eb) in &map_b {
        match map_a.get(sel) {
            Some(ea) => {
                // Raw (viewport-relative) deltas are returned to the
                // caller verbatim — that's the documented `BboxChange`
                // contract and `motion.diff` consumers expect it.
                // But the gate (whether to emit a change record at
                // all) uses document-relative deltas so a between-
                // frame scroll doesn't flood `bbox_changes` with every
                // visible element on the page.
                let dx = eb.bbox[0] - ea.bbox[0];
                let dy = eb.bbox[1] - ea.bbox[1];
                let dw = eb.bbox[2] - ea.bbox[2];
                let dh = eb.bbox[3] - ea.bbox[3];
                let dx_doc = (eb.bbox[0] + b.scroll_x) - (ea.bbox[0] + a.scroll_x);
                let dy_doc = (eb.bbox[1] + b.scroll_y) - (ea.bbox[1] + a.scroll_y);
                let opacity_delta = eb.opacity - ea.opacity;
                if dx_doc.abs() > 0.5
                    || dy_doc.abs() > 0.5
                    || dw.abs() > 0.5
                    || dh.abs() > 0.5
                    || opacity_delta.abs() > 0.01
                {
                    bbox_changes.push(BboxChange {
                        selector: sel.to_string(),
                        from: ea.bbox,
                        to: eb.bbox,
                        dx,
                        dy,
                        dw,
                        dh,
                        opacity_delta,
                    });
                }
                if ea.transform != eb.transform {
                    style_changes.push(StyleChange {
                        selector: sel.to_string(),
                        property: "transform".into(),
                        from: ea.transform.clone().unwrap_or_else(|| "none".into()),
                        to: eb.transform.clone().unwrap_or_else(|| "none".into()),
                    });
                }
                if ea.background_color != eb.background_color {
                    style_changes.push(StyleChange {
                        selector: sel.to_string(),
                        property: "background-color".into(),
                        from: ea.background_color.clone().unwrap_or_default(),
                        to: eb.background_color.clone().unwrap_or_default(),
                    });
                }
                if ea.color != eb.color {
                    style_changes.push(StyleChange {
                        selector: sel.to_string(),
                        property: "color".into(),
                        from: ea.color.clone().unwrap_or_default(),
                        to: eb.color.clone().unwrap_or_default(),
                    });
                }
                if ea.display != eb.display {
                    style_changes.push(StyleChange {
                        selector: sel.to_string(),
                        property: "display".into(),
                        from: ea.display.clone(),
                        to: eb.display.clone(),
                    });
                }
            }
            None => appeared.push(sel.to_string()),
        }
    }
    for sel in map_a.keys() {
        if !map_b.contains_key(sel) {
            disappeared.push(sel.to_string());
        }
    }
    (bbox_changes, style_changes, appeared, disappeared)
}

/// Return the selectors that newly appeared (b minus a, not in
/// `hidden_selectors[a]`) and the selectors that disappeared (a minus
/// b, not in `hidden_selectors[b]`) between two adjacent frames. The
/// `hidden_selectors` exclusion mirrors `classify_transition`'s
/// Appearance / Disappearance → Fade reclassification so this list
/// only reports *real* DOM-new / DOM-removed events. Used to scope
/// `forbidden_kinds: ["appearance" | "disappearance"]` to the
/// `expected_targets` the agent actually named, so scrolling that
/// pushes unrelated sections off the viewport does not false-fail
/// the gate.
fn appeared_disappeared_between(a: &Frame, b: &Frame) -> (Vec<String>, Vec<String>) {
    let (sa, sb) = match (a.layout_snapshot.as_ref(), b.layout_snapshot.as_ref()) {
        (Some(a), Some(b)) => (a, b),
        _ => return (Vec::new(), Vec::new()),
    };
    use std::collections::HashSet;
    let set_a: HashSet<&str> = sa.elements.iter().map(|e| e.selector.as_str()).collect();
    let set_b: HashSet<&str> = sb.elements.iter().map(|e| e.selector.as_str()).collect();
    let hidden_a: HashSet<&str> = sa.hidden_selectors.iter().map(String::as_str).collect();
    let hidden_b: HashSet<&str> = sb.hidden_selectors.iter().map(String::as_str).collect();
    let mut appeared = Vec::new();
    for sel in &set_b {
        if !set_a.contains(sel) && !hidden_a.contains(sel) {
            appeared.push(sel.to_string());
        }
    }
    let mut disappeared = Vec::new();
    for sel in &set_a {
        if !set_b.contains(sel) && !hidden_b.contains(sel) {
            disappeared.push(sel.to_string());
        }
    }
    (appeared, disappeared)
}

fn classify_transition(a: &Frame, b: &Frame) -> Vec<MotionCategory> {
    let (sa, sb) = match (a.layout_snapshot.as_ref(), b.layout_snapshot.as_ref()) {
        (Some(a), Some(b)) => (a, b),
        _ => return Vec::new(),
    };
    use std::collections::{HashMap, HashSet};
    let map_a: HashMap<&str, &crate::model::ElementProbe> = sa
        .elements
        .iter()
        .map(|e| (e.selector.as_str(), e))
        .collect();
    let map_b: HashMap<&str, &crate::model::ElementProbe> = sb
        .elements
        .iter()
        .map(|e| (e.selector.as_str(), e))
        .collect();
    // Selectors the visibility filter dropped from `elements` on each
    // side. An element that's in `hidden_selectors` on `a` but in
    // `elements` on `b` is not a DOM-new appearance — it's the same
    // node fading in from `opacity:0` (or being shown via
    // `display:none → block`). Classify those as `fade` rather than
    // `appearance` so the verdict doesn't depend on whether the caller
    // passed `expected_targets`.
    let hidden_a: HashSet<&str> = sa.hidden_selectors.iter().map(String::as_str).collect();
    let hidden_b: HashSet<&str> = sb.hidden_selectors.iter().map(String::as_str).collect();
    let mut kinds: Vec<MotionCategory> = Vec::new();
    let push = |k: MotionCategory, kinds: &mut Vec<MotionCategory>| {
        if !kinds.contains(&k) {
            kinds.push(k);
        }
    };
    for (sel, eb) in &map_b {
        match map_a.get(sel) {
            Some(ea) => {
                // Document-relative bbox so a between-frame scroll
                // doesn't classify every visible element as Translate
                // (see moved_selectors_between for the same fix).
                let dx = (eb.bbox[0] + sb.scroll_x) - (ea.bbox[0] + sa.scroll_x);
                let dy = (eb.bbox[1] + sb.scroll_y) - (ea.bbox[1] + sa.scroll_y);
                let dw = eb.bbox[2] - ea.bbox[2];
                let dh = eb.bbox[3] - ea.bbox[3];
                if dx.abs() > 0.5 || dy.abs() > 0.5 {
                    push(MotionCategory::Translate, &mut kinds);
                }
                if dw.abs() > 0.5 || dh.abs() > 0.5 {
                    push(MotionCategory::Scale, &mut kinds);
                    push(MotionCategory::Layout, &mut kinds);
                }
                let dop = eb.opacity - ea.opacity;
                if dop.abs() > 0.01 {
                    push(MotionCategory::Fade, &mut kinds);
                }
                if ea.transform != eb.transform {
                    // Browsers serialize transforms as `matrix(a,b,c,d,tx,ty)`
                    // or `matrix3d(...)`. Decompose to extract real rotation
                    // and scale instead of doing substring matching on the
                    // serialized form (which would never contain "rotate").
                    let (ra, sxa, sya, txa, tya) = ea
                        .transform
                        .as_deref()
                        .and_then(parse_transform_2d)
                        .map(decompose_2d)
                        .unwrap_or((0.0, 1.0, 1.0, 0.0, 0.0));
                    let (rb, sxb, syb, txb, tyb) = eb
                        .transform
                        .as_deref()
                        .and_then(parse_transform_2d)
                        .map(decompose_2d)
                        .unwrap_or((0.0, 1.0, 1.0, 0.0, 0.0));
                    if (ra - rb).abs() > 1.0 {
                        push(MotionCategory::Rotate, &mut kinds);
                    }
                    if (sxa - sxb).abs() > 0.01 || (sya - syb).abs() > 0.01 {
                        push(MotionCategory::Scale, &mut kinds);
                    }
                    if (txa - txb).abs() > 0.5 || (tya - tyb).abs() > 0.5 {
                        push(MotionCategory::Translate, &mut kinds);
                    }
                    // If parsing failed entirely and we still see a non-empty
                    // change, fall back to Translate so the agent still gets
                    // a signal that *something* moved.
                    if ea
                        .transform
                        .as_deref()
                        .and_then(parse_transform_2d)
                        .is_none()
                        && eb
                            .transform
                            .as_deref()
                            .and_then(parse_transform_2d)
                            .is_none()
                    {
                        let s_b = eb.transform.as_deref().unwrap_or("");
                        if !s_b.is_empty() && s_b != "none" {
                            push(MotionCategory::Translate, &mut kinds);
                        }
                    }
                }
                if ea.color != eb.color || ea.background_color != eb.background_color {
                    push(MotionCategory::ColorChange, &mut kinds);
                }
                if ea.text_content_preview != eb.text_content_preview
                    && (ea.text_content_preview.is_some() || eb.text_content_preview.is_some())
                {
                    // A Stats count-up rolling `0 → 247`, a typewriter
                    // writing characters, a label flipping between
                    // strings — none of these touch bbox / opacity /
                    // transform, so without this branch the classifier
                    // sees the element as static. The probe only
                    // collects text_content_preview for leaf-ish
                    // elements (<=1 child), so this fires precisely on
                    // the leaves where text actually changes.
                    push(MotionCategory::TextChange, &mut kinds);
                }
            }
            None => {
                if hidden_a.contains(*sel) {
                    // The element existed in `a`'s DOM but was filtered
                    // out (opacity:0 / display:none / zero-bbox). It is
                    // now visible in `b` — that's a fade-in, not a new
                    // appearance.
                    push(MotionCategory::Fade, &mut kinds);
                } else {
                    push(MotionCategory::Appearance, &mut kinds);
                }
            }
        }
    }
    for sel in map_a.keys() {
        if !map_b.contains_key(sel) {
            if hidden_b.contains(*sel) {
                // Element became hidden (filtered out) on `b`, but it is
                // still in the DOM. Treat as fade-out, not disappearance.
                push(MotionCategory::Fade, &mut kinds);
            } else {
                push(MotionCategory::Disappearance, &mut kinds);
            }
        }
    }
    kinds
}

/// Parse a CSS `transform` computed-style string into 2D matrix components
/// `(a, b, c, d, tx, ty)`. Supports both `matrix(...)` (6 args) and
/// `matrix3d(...)` (16 args; we project to the 2D subset). Returns `None` for
/// any other form (including `"none"`).
fn parse_transform_2d(s: &str) -> Option<(f64, f64, f64, f64, f64, f64)> {
    let s = s.trim();
    if s == "none" || s.is_empty() {
        return None;
    }
    if let Some(inner) = s.strip_prefix("matrix(").and_then(|x| x.strip_suffix(')')) {
        let parts: Vec<f64> = inner
            .split(',')
            .filter_map(|p| p.trim().parse().ok())
            .collect();
        if parts.len() == 6 {
            return Some((parts[0], parts[1], parts[2], parts[3], parts[4], parts[5]));
        }
    }
    if let Some(inner) = s
        .strip_prefix("matrix3d(")
        .and_then(|x| x.strip_suffix(')'))
    {
        let parts: Vec<f64> = inner
            .split(',')
            .filter_map(|p| p.trim().parse().ok())
            .collect();
        if parts.len() == 16 {
            // 4x4 column-major; the 2D subset is in cols 0,1 and the
            // translate components at indices 12,13.
            return Some((parts[0], parts[1], parts[4], parts[5], parts[12], parts[13]));
        }
    }
    None
}

/// Decompose a 2D affine matrix into `(rotation_deg, scale_x, scale_y, tx, ty)`.
/// Rotation handles negative determinants conservatively.
fn decompose_2d(m: (f64, f64, f64, f64, f64, f64)) -> (f64, f64, f64, f64, f64) {
    let (a, b, _c, d, tx, ty) = m;
    let sx = (a * a + b * b).sqrt();
    let sy = (m.2 * m.2 + d * d).sqrt();
    let rotation_deg = b.atan2(a).to_degrees();
    (rotation_deg, sx, sy, tx, ty)
}

fn scroll_range_verdict(
    expected_start: f64,
    expected_end: f64,
    observed_start: f64,
    observed_end: f64,
) -> &'static str {
    let eps_lo = expected_start.min(expected_end);
    let eps_hi = expected_start.max(expected_end);
    let obs_lo = observed_start.min(observed_end);
    let obs_hi = observed_start.max(observed_end);
    let tol = 0.05;
    if obs_hi < eps_lo - tol || obs_lo > eps_hi + tol {
        "outside-expected"
    } else if obs_lo > eps_lo + tol && obs_hi < eps_hi - tol {
        "narrower-than-expected"
    } else if obs_lo < eps_lo - tol || obs_hi > eps_hi + tol {
        "wider-than-expected"
    } else {
        "match"
    }
}

fn build_intent_match(
    intent: &EpisodeIntent,
    detected: &[MotionCategory],
    moved_selectors: &[String],
    coverage_ms: f64,
    per_target_easing: &[crate::model::TargetEasing],
    appeared_selectors: &[String],
    disappeared_selectors: &[String],
) -> IntentMatchReport {
    let expected_seen: Vec<MotionCategory> = intent
        .expected_kinds
        .iter()
        .filter(|k| detected.contains(k))
        .copied()
        .collect();
    let expected_missing: Vec<MotionCategory> = intent
        .expected_kinds
        .iter()
        .filter(|k| !detected.contains(k))
        .copied()
        .collect();
    // forbidden_kinds is scoped to expected_targets when those are
    // declared: an Appearance / Disappearance on an unrelated element
    // (a scrolled-off sibling section, an off-viewport background
    // panel) shouldn't false-fail a contract that named specific
    // targets. For other kinds (translate / fade / etc.) we mirror
    // the same scope using moved_selectors ∩ expected_targets —
    // matched_selectors keeps the comparison Element.matches()-aware,
    // not string-equality.
    // When expected_targets is empty the contract is "page-wide";
    // we keep the original behaviour (forbidden over all detected).
    let forbidden_seen: Vec<MotionCategory> = if intent.expected_targets.is_empty() {
        intent
            .forbidden_kinds
            .iter()
            .filter(|k| detected.contains(k))
            .copied()
            .collect()
    } else {
        // Pre-compute set of expected_targets for cheap membership.
        let targets_matched_in_appeared = appeared_selectors
            .iter()
            .any(|s| intent.expected_targets.iter().any(|t| t == s));
        let targets_matched_in_disappeared = disappeared_selectors
            .iter()
            .any(|s| intent.expected_targets.iter().any(|t| t == s));
        let targets_moved = moved_selectors
            .iter()
            .any(|s| intent.expected_targets.iter().any(|t| t == s));
        intent
            .forbidden_kinds
            .iter()
            .filter(|k| {
                if !detected.contains(k) {
                    return false;
                }
                match k {
                    MotionCategory::Appearance => targets_matched_in_appeared,
                    MotionCategory::Disappearance => targets_matched_in_disappeared,
                    // Movement-like kinds are scoped via moved_selectors
                    // intersection: did any expected_target itself move
                    // in a way that triggered this kind?
                    _ => targets_moved,
                }
            })
            .copied()
            .collect()
    };
    let targets_seen: Vec<String> = intent
        .expected_targets
        .iter()
        .filter(|sel| moved_selectors.iter().any(|m| m == *sel))
        .cloned()
        .collect();
    let targets_missing: Vec<String> = intent
        .expected_targets
        .iter()
        .filter(|sel| !moved_selectors.iter().any(|m| m == *sel))
        .cloned()
        .collect();
    // When `expected_targets` are declared and `per_target_easing`
    // resolved their onset/settle, measure the observed span against
    // those targets only, so an ambient background animation (a
    // long-loop @keyframes drift, an infinite marquee) does not
    // inflate the verdict. Fall back to the sample_plan span when
    // no target's motion span is resolvable — otherwise a 900ms
    // reveal in a 1900ms sample window reads as too-long even though
    // the targets settled correctly.
    let target_span_ms: Option<f64> = {
        let mut onsets: Vec<f64> = Vec::new();
        let mut settles: Vec<f64> = Vec::new();
        for te in per_target_easing {
            let is_target = intent
                .expected_targets
                .iter()
                .any(|sel| sel == &te.selector);
            if !is_target {
                continue;
            }
            if let Some(o) = te.onset_ms {
                onsets.push(o);
            }
            if let Some(s) = te.settle_ms {
                settles.push(s);
            }
        }
        if !onsets.is_empty() && !settles.is_empty() {
            let min_onset = onsets.iter().cloned().fold(f64::INFINITY, f64::min);
            let max_settle = settles.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            let span = (max_settle - min_onset).max(0.0);
            if span > 0.0 {
                Some(span)
            } else {
                None
            }
        } else {
            None
        }
    };
    let duration_match = intent.expected_duration_ms.map(|expected| {
        let observed = target_span_ms.unwrap_or(coverage_ms);
        let ratio = observed / expected;
        // When `target_span_ms` resolves, the comparison is "did the
        // declared motion run no longer than expected?" — the lower
        // bound is intentionally permissive: sample-plan granularity
        // ( `target_times_ms` spaced at 150ms / 300ms ) routinely
        // records `settle_ms` at the first sample whose progress
        // crossed 0.95, which can fall well before the declared
        // duration completes (a 600ms ease-out-cubic typically reads
        // settle around t≈450..510ms). Treating that as "too-short"
        // would false-fail every legitimate motion that the agent
        // sized correctly. We only gate on `too-long` — meaning the
        // motion actually overran its declared duration. Sample-plan
        // span fallback keeps the original two-sided gate because in
        // that mode observed and expected both measure the same span.
        let verdict = if target_span_ms.is_some() {
            if ratio <= 1.2 {
                "match"
            } else {
                "too-long"
            }
        } else if (0.8..=1.2).contains(&ratio) {
            "match"
        } else if ratio < 0.8 {
            "too-short"
        } else {
            "too-long"
        };
        DurationMatch {
            expected_ms: expected,
            observed_coverage_ms: observed,
            ratio,
            verdict: verdict.into(),
        }
    });
    let passes = expected_missing.is_empty()
        && forbidden_seen.is_empty()
        && targets_missing.is_empty()
        && duration_match
            .as_ref()
            .map(|d| d.verdict == "match")
            .unwrap_or(true);
    IntentMatchReport {
        description: intent.description.clone(),
        expected_kinds_seen: expected_seen,
        expected_kinds_missing: expected_missing,
        forbidden_kinds_seen: forbidden_seen,
        expected_targets_seen: targets_seen,
        expected_targets_missing: targets_missing,
        duration_match,
        easing_match: None,
        scroll_range_match: None,
        passes,
    }
}

/// Return the selectors whose `bbox` / `opacity` / `transform` changed
/// meaningfully between `a` and `b`. Used to populate
/// `AnimationAssessment.moved_selectors` and to evaluate
/// `intent.expected_targets` against actual evidence.
fn moved_selectors_between(a: &Frame, b: &Frame) -> Vec<String> {
    let (sa, sb) = match (a.layout_snapshot.as_ref(), b.layout_snapshot.as_ref()) {
        (Some(a), Some(b)) => (a, b),
        _ => return Vec::new(),
    };
    use std::collections::HashMap;
    let map_a: HashMap<&str, &crate::model::ElementProbe> = sa
        .elements
        .iter()
        .map(|e| (e.selector.as_str(), e))
        .collect();
    let mut out: Vec<String> = Vec::new();
    for eb in &sb.elements {
        let ea = match map_a.get(eb.selector.as_str()) {
            Some(ea) => *ea,
            None => {
                // Selector appeared between frames; that's a move too.
                out.push(eb.selector.clone());
                out.extend(eb.matched_selectors.iter().cloned());
                continue;
            }
        };
        // Use document-relative bbox (add the snapshot's scroll
        // offset) so a `trigger.scroll` between frames doesn't tag
        // every visible element as "moved" — the page just moved, the
        // elements stayed where they were on the document.
        let dx = ((eb.bbox[0] + sb.scroll_x) - (ea.bbox[0] + sa.scroll_x)).abs();
        let dy = ((eb.bbox[1] + sb.scroll_y) - (ea.bbox[1] + sa.scroll_y)).abs();
        let dw = (eb.bbox[2] - ea.bbox[2]).abs();
        let dh = (eb.bbox[3] - ea.bbox[3]).abs();
        let dop = (eb.opacity - ea.opacity).abs();
        let transform_changed = ea.transform != eb.transform;
        let text_changed = ea.text_content_preview != eb.text_content_preview
            && (ea.text_content_preview.is_some() || eb.text_content_preview.is_some());
        if dx > 0.5
            || dy > 0.5
            || dw > 0.5
            || dh > 0.5
            || dop > 0.01
            || transform_changed
            || text_changed
        {
            out.push(eb.selector.clone());
            // Also surface the caller's own CSS selectors that this moved
            // element satisfies, so `intent.expected_targets` is graded by
            // real `Element.matches()` semantics rather than by string
            // equality against the synthesized `#id`-priority notation.
            out.extend(eb.matched_selectors.iter().cloned());
        }
    }
    // Selectors that disappeared also count as movement.
    let map_b: std::collections::HashSet<&str> =
        sb.elements.iter().map(|e| e.selector.as_str()).collect();
    for ea in &sa.elements {
        if !map_b.contains(ea.selector.as_str()) {
            out.push(ea.selector.clone());
            out.extend(ea.matched_selectors.iter().cloned());
        }
    }
    out
}

/// Catch a sudden positional discontinuity (an element "teleporting" in a
/// single interval) from the per-element bbox-center series. This is
/// independent of the whole-page pixel-delta CV: a small element jumping
/// 500px barely moves the changed-pixel ratio, so the pixel pass misses
/// it. Here a moving element whose displacement in one interval is a
/// strong outlier versus its own other intervals (and exceeds an absolute
/// px floor) is flagged — robust to coarse sampling and to uniform pixel
/// deltas. Smooth large motion (consistent displacement across intervals)
/// is NOT flagged because it is not an outlier.
fn detect_positional_jank(frames: &[&Frame]) -> Vec<JankEvent> {
    use std::collections::HashMap;
    if frames.len() < 3 {
        return Vec::new();
    }
    let n_int = frames.len() - 1;
    let vw = frames
        .iter()
        .find_map(|f| f.layout_snapshot.as_ref().map(|s| s.viewport_width))
        .filter(|w| *w > 0.0)
        .unwrap_or(1280.0);
    // selector -> per-interval displacement (None when the element is absent
    // at either endpoint of that interval; appearance / disappearance is
    // handled by moved_selectors_between, not treated as a teleport here).
    let mut series: HashMap<&str, Vec<Option<f64>>> = HashMap::new();
    for k in 0..n_int {
        let (sa, sb) = match (
            frames[k].layout_snapshot.as_ref(),
            frames[k + 1].layout_snapshot.as_ref(),
        ) {
            (Some(a), Some(b)) => (a, b),
            _ => continue,
        };
        let mut map_a: HashMap<&str, &crate::model::ElementProbe> = HashMap::new();
        for e in &sa.elements {
            map_a.entry(e.selector.as_str()).or_insert(e);
        }
        for eb in &sb.elements {
            if let Some(ea) = map_a.get(eb.selector.as_str()) {
                // document-relative centres so a `trigger.scroll`
                // between frames doesn't surface every visible element
                // as a positional-teleport (same artefact as
                // overshoot's; see compute_easing_stagger_overshoot).
                let ca = (
                    ea.bbox[0] + ea.bbox[2] / 2.0 + sa.scroll_x,
                    ea.bbox[1] + ea.bbox[3] / 2.0 + sa.scroll_y,
                );
                let cb = (
                    eb.bbox[0] + eb.bbox[2] / 2.0 + sb.scroll_x,
                    eb.bbox[1] + eb.bbox[3] / 2.0 + sb.scroll_y,
                );
                let disp = ((cb.0 - ca.0).powi(2) + (cb.1 - ca.1).powi(2)).sqrt();
                series
                    .entry(eb.selector.as_str())
                    .or_insert_with(|| vec![None; n_int])[k] = Some(disp);
            }
        }
    }
    const ABS_FLOOR_PX: f64 = 48.0;
    const RATIO: f64 = 4.0;
    let mut by_interval: HashMap<usize, JankEvent> = HashMap::new();
    for disps in series.values() {
        let present: Vec<(usize, f64)> = disps
            .iter()
            .enumerate()
            .filter_map(|(i, d)| d.map(|v| (i, v)))
            .collect();
        if present.len() < 3 {
            continue;
        }
        for &(idx, d) in &present {
            if d < ABS_FLOOR_PX {
                continue;
            }
            let mut others: Vec<f64> = present
                .iter()
                .filter(|(i, _)| *i != idx)
                .map(|(_, v)| *v)
                .collect();
            if others.is_empty() {
                continue;
            }
            others.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let median = others[others.len() / 2];
            let base = median.max(1.0);
            if d > RATIO * base {
                let ratio = d / base;
                let ev = JankEvent {
                    from_t_ms: frames[idx].t_ms,
                    to_t_ms: frames[idx + 1].t_ms,
                    delta_ratio: (d / vw).clamp(0.0, 1.0),
                    z_score: ratio,
                    kind: JankKind::PositionalTeleport,
                };
                by_interval
                    .entry(idx)
                    .and_modify(|cur| {
                        if ratio > cur.z_score {
                            *cur = ev.clone();
                        }
                    })
                    .or_insert(ev);
            }
        }
    }
    let mut out: Vec<JankEvent> = by_interval.into_values().collect();
    out.sort_by(|a, b| {
        a.from_t_ms
            .partial_cmp(&b.from_t_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

async fn screenshot(page: &Page, format: ImageFormat, full_page: bool) -> Result<Vec<u8>> {
    let cdp_format = match format {
        ImageFormat::Png => CaptureScreenshotFormat::Png,
        ImageFormat::Jpeg => CaptureScreenshotFormat::Jpeg,
    };
    let params = ScreenshotParams::builder()
        .format(cdp_format)
        .full_page(full_page)
        .build();
    // Bounded so a pathological page can never hang the whole tool —
    // the same "never block the agent indefinitely" guarantee
    // `advance()` enforces for its CDP wait. `Page.captureScreenshot`
    // blocks indefinitely when no compositor frame has been committed
    // since the virtual clock was frozen — e.g. a CSS `@keyframes`
    // animation armed by a trigger with no intervening `clock.advance`.
    // Surface that as a typed error (no fake frame — never lie to the
    // caller) instead of deadlocking the agent / CI forever.
    const CAPTURE_TIMEOUT_MS: u64 = 8000;
    match tokio::time::timeout(
        std::time::Duration::from_millis(CAPTURE_TIMEOUT_MS),
        page.screenshot(params),
    )
    .await
    {
        Ok(r) => r.map_err(Error::browser),
        Err(_) => Err(Error::browser(format!(
            "screenshot timed out after {CAPTURE_TIMEOUT_MS} ms: the page produced no \
             compositor frame at the current virtual time. Known cause: a CSS \
             `@keyframes` animation armed by a trigger with no intervening \
             `clock.advance` before this capture — advance the virtual clock by \
             a few ms after arming the animation before requesting a frame."
        ))),
    }
}

/// Resolve a target selector to viewport-space coordinates `(x, y)` at
/// the element's bounding-box centre. Used by the CDP-input trigger path
/// so the synthetic mouse event lands on the element the agent named
/// rather than at `(0, 0)`.
async fn resolve_target_center_px(page: &Page, target: &TargetSpec) -> Result<(f64, f64)> {
    let js = if target.frame_path.is_empty() {
        format!(
            "(function () {{ const el = document.querySelector({sel}); if (!el) return null; const r = el.getBoundingClientRect(); return JSON.stringify({{ x: r.left + r.width / 2, y: r.top + r.height / 2 }}); }})()",
            sel = serde_json::to_string(&target.selector).unwrap_or_else(|_| "\"\"".into())
        )
    } else {
        let action = "const r = el.getBoundingClientRect(); return JSON.stringify({ x: r.left + r.width / 2, y: r.top + r.height / 2 });";
        iframe_action_js(&target.frame_path, &target.selector, action)
    };
    let result = page.evaluate(js.as_str()).await.map_err(Error::browser)?;
    let json: Option<String> = result.into_value().ok();
    let json = json.ok_or_else(|| {
        Error::invalid(format!(
            "CDP-input trigger: selector `{}` did not resolve to a visible element",
            target.selector
        ))
    })?;
    #[derive(serde::Deserialize)]
    struct Pt {
        x: f64,
        y: f64,
    }
    let pt: Pt = serde_json::from_str(&json)
        .map_err(|e| Error::browser(format!("parse target center: {e}")))?;
    Ok((pt.x, pt.y))
}

/// Force `forced_pseudo_classes` on the element matched by `target` via
/// CDP `CSS.forcePseudoState`. Lets `trigger.hover input_mode: cdp`
/// make CSS transitions tick under the virtual clock — without this
/// the transition stays at `currentTime: 0` even with the
/// compositor cursor parked over the element (the transition's
/// internal scheduler isn't pinned to `document.timeline`). Pairs
/// the `forcePseudoState` with the normal `Input.dispatchMouseEvent`
/// so coordinate-driven listeners still observe the cursor.
async fn force_pseudo_state(page: &Page, target: &TargetSpec, forced: &[&str]) -> Result<()> {
    use chromiumoxide::cdp::browser_protocol::css::{
        EnableParams as CssEnableParams, ForcePseudoStateParams,
    };
    use chromiumoxide::cdp::browser_protocol::dom::{GetDocumentParams, QuerySelectorParams};
    // `CSS.enable` is idempotent — page setup might or might not have
    // enabled it already. Calling it again returns Ok with no
    // observable side effect.
    page.execute(CssEnableParams::default())
        .await
        .map_err(Error::browser)?;
    let doc = page
        .execute(GetDocumentParams::default())
        .await
        .map_err(Error::browser)?;
    let root_id = doc.root.node_id;
    let qsel = QuerySelectorParams::builder()
        .node_id(root_id)
        .selector(target.selector.clone())
        .build()
        .map_err(|e| Error::browser(format!("build CSS.forcePseudoState querySelector: {e}")))?;
    let q = page.execute(qsel).await.map_err(Error::browser)?;
    let node_id = q.node_id;
    // `QuerySelector` returns NodeId 0 when the selector matches
    // nothing. Force-on-null is a no-op that just clutters logs; bail.
    let node_id_raw: i64 = node_id.inner().to_owned();
    if node_id_raw == 0 {
        return Err(Error::invalid(format!(
            "CSS.forcePseudoState: selector `{}` did not match any node",
            target.selector
        )));
    }
    let fps = ForcePseudoStateParams::builder()
        .node_id(node_id)
        .forced_pseudo_classes(forced.iter().map(|s| (*s).to_string()))
        .build()
        .map_err(|e| Error::browser(format!("build CSS.forcePseudoState: {e}")))?;
    page.execute(fps).await.map_err(Error::browser)?;
    Ok(())
}

/// Trigger dispatch via CDP `Input.dispatchMouseEvent` / `Input.insertText` —
/// runs the input through Chromium's compositor pipeline so CSS `:hover`,
/// `pointerdown` / `pointerup`, and coordinate-driven listeners observe a
/// genuine event.  Cross-origin iframes are out of scope.  The caller
/// (`Session::trigger`) is responsible for flushing the paint pipeline
/// (advance the virtual clock by ~16ms) after this returns so the next
/// `Page.captureScreenshot` doesn't hang on a pending frame.
async fn dispatch_trigger_on_cdp(page: &Page, kind: &TriggerKind) -> Result<()> {
    use chromiumoxide::cdp::browser_protocol::input::{
        DispatchMouseEventParams, DispatchMouseEventType, MouseButton,
    };
    match kind {
        TriggerKind::Click { target } => {
            let (x, y) = resolve_target_center_px(page, target).await?;
            // Move into position first so `mouseenter` / `mouseover` fire
            // on the way in, then press + release.
            let mv = DispatchMouseEventParams::builder()
                .r#type(DispatchMouseEventType::MouseMoved)
                .x(x)
                .y(y)
                .build()
                .map_err(|e| Error::browser(format!("build mouseMoved: {e}")))?;
            page.execute(mv).await.map_err(Error::browser)?;
            let press = DispatchMouseEventParams::builder()
                .r#type(DispatchMouseEventType::MousePressed)
                .x(x)
                .y(y)
                .button(MouseButton::Left)
                .buttons(1)
                .click_count(1)
                .build()
                .map_err(|e| Error::browser(format!("build mousePressed: {e}")))?;
            page.execute(press).await.map_err(Error::browser)?;
            let release = DispatchMouseEventParams::builder()
                .r#type(DispatchMouseEventType::MouseReleased)
                .x(x)
                .y(y)
                .button(MouseButton::Left)
                .click_count(1)
                .build()
                .map_err(|e| Error::browser(format!("build mouseReleased: {e}")))?;
            page.execute(release).await.map_err(Error::browser)?;
        }
        TriggerKind::Hover { target } => {
            let (x, y) = resolve_target_center_px(page, target).await?;
            let mv = DispatchMouseEventParams::builder()
                .r#type(DispatchMouseEventType::MouseMoved)
                .x(x)
                .y(y)
                .build()
                .map_err(|e| Error::browser(format!("build mouseMoved: {e}")))?;
            page.execute(mv).await.map_err(Error::browser)?;
            // `Input.dispatchMouseEvent` activates the `:hover`
            // pseudo-class via the compositor cursor, but on a paused
            // virtual clock the CSS transition that the hover starts
            // does NOT actually progress its `currentTime` from
            // `clock.advance` — the transition's internal scheduler
            // doesn't tick on the same timeline. To make hover
            // transitions verifiable, additionally force the `:hover`
            // pseudo-class via `CSS.forcePseudoState`, which Chromium
            // routes through the style engine: the transition starts
            // from a true style change and its time origin pins to
            // the virtual clock just like a `@keyframes` animation.
            // The mouseMoved call above is kept so the cursor itself
            // is also positioned (some sites read `mousemove` x/y).
            if let Err(e) = force_pseudo_state(page, target, &["hover"]).await {
                // The pseudo-state path is best-effort — if a
                // selector doesn't resolve to a single nodeId we
                // still have the dispatchMouseEvent above, so the
                // common case still triggers a `:hover`. Log silently.
                tracing::debug!(?e, "force_pseudo_state hover skipped");
            }
        }
        TriggerKind::Type { .. } => {
            // CDP `Input.insertText` is the equivalent path for text but
            // requires the target to already have focus.  Fall back to JS
            // dispatch which can `el.focus()` and assign `value` directly;
            // that's structurally equivalent to a real keyboard event for
            // every observable side effect (input / change events fire).
            dispatch_trigger_on(page, kind).await?;
        }
        TriggerKind::Scroll { .. } | TriggerKind::Evaluate { .. } => {
            // Scroll and evaluate have no compositor-input flavour worth
            // synthesising — fall back to the JS path.
            dispatch_trigger_on(page, kind).await?;
        }
    }
    Ok(())
}

/// Read and drain the preload LoAF buffer from the page.  Returns
/// `Ok(vec![])` when the API is unavailable in the current renderer.
async fn collect_runtime_jank(page: &Page) -> Result<Vec<crate::model::RuntimeJankEvent>> {
    let js = "(function () { try { const s = window.__motionlens; if (!s || !Array.isArray(s.loafEntries)) return '[]'; const out = JSON.stringify(s.loafEntries); s.loafEntries = []; return out; } catch (e) { return '[]'; } })()";
    let result = page.evaluate(js).await.map_err(Error::browser)?;
    let json: String = result.into_value().unwrap_or_else(|_| "[]".into());
    #[derive(serde::Deserialize)]
    struct Raw {
        #[serde(rename = "startTime")]
        start_time: f64,
        duration: f64,
        #[serde(rename = "renderStart", default)]
        render_start: f64,
        #[serde(rename = "styleAndLayoutStart", default)]
        style_and_layout_start: f64,
        #[serde(rename = "blockingDuration", default)]
        blocking_duration: f64,
    }
    let entries: Vec<Raw> = serde_json::from_str(&json).unwrap_or_default();
    Ok(entries
        .into_iter()
        .map(|e| crate::model::RuntimeJankEvent {
            start_time_ms: e.start_time,
            duration_ms: e.duration,
            render_start_ms: e.render_start,
            style_and_layout_start_ms: e.style_and_layout_start,
            blocking_duration_ms: e.blocking_duration,
        })
        .collect())
}

async fn dispatch_trigger_on(page: &Page, kind: &TriggerKind) -> Result<()> {
    // Triggers run through JS DOM APIs (`el.click()`, `dispatchEvent`,
    // direct `value` assignment) rather than CDP `Input.dispatchMouseEvent` /
    // `Input.insertText`. The reason is structural: CDP Input enqueues real
    // input events into Chromium's compositor pipeline, which then waits
    // for the next paint frame to complete. Under our paused virtual clock
    // there IS no next paint, so the subsequent `Page.captureScreenshot`
    // never returns. JS-level dispatch runs the handler synchronously on
    // the renderer's main thread and doesn't block the screenshot path.
    //
    // The trade-off is that CSS `:hover`, `pointerdown`/`up`, and
    // real-coordinate-driven listeners that only fire on a true
    // compositor input won't be observed. Closing that gap requires
    // pairing CDP Input with `HeadlessExperimental.beginFrame` to
    // force one paint after each input.
    //
    // Nested iframes go through `iframe_action_js` to walk the frame chain;
    // cross-origin iframes are out of scope.
    match kind {
        TriggerKind::Click { target } => {
            if target.frame_path.is_empty() {
                let el = page
                    .find_element(target.selector.as_str())
                    .await
                    .map_err(Error::browser)?;
                el.call_js_fn("function() { this.click(); }", false)
                    .await
                    .map_err(Error::browser)?;
            } else {
                let js = iframe_action_js(&target.frame_path, &target.selector, "el.click();");
                page.evaluate(js.as_str()).await.map_err(Error::browser)?;
            }
        }
        TriggerKind::Hover { target } => {
            let action = "el.dispatchEvent(new MouseEvent('mouseenter', { bubbles: true })); el.dispatchEvent(new MouseEvent('mouseover', { bubbles: true }));";
            if target.frame_path.is_empty() {
                let el = page
                    .find_element(target.selector.as_str())
                    .await
                    .map_err(Error::browser)?;
                el.call_js_fn(
                    "function() { this.dispatchEvent(new MouseEvent('mouseenter', { bubbles: true })); this.dispatchEvent(new MouseEvent('mouseover', { bubbles: true })); }",
                    false,
                )
                .await
                .map_err(Error::browser)?;
            } else {
                let js = iframe_action_js(&target.frame_path, &target.selector, action);
                page.evaluate(js.as_str()).await.map_err(Error::browser)?;
            }
        }
        TriggerKind::Type { target, text } => {
            let js_text = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
            if target.frame_path.is_empty() {
                let el = page
                    .find_element(target.selector.as_str())
                    .await
                    .map_err(Error::browser)?;
                let js = format!(
                    "function() {{ this.focus(); this.value = {text}; this.dispatchEvent(new Event('input', {{ bubbles: true }})); this.dispatchEvent(new Event('change', {{ bubbles: true }})); }}",
                    text = js_text
                );
                el.call_js_fn(js.as_str(), false)
                    .await
                    .map_err(Error::browser)?;
            } else {
                let action = format!(
                    "el.focus(); el.value = {text}; el.dispatchEvent(new Event('input', {{ bubbles: true }})); el.dispatchEvent(new Event('change', {{ bubbles: true }}));",
                    text = js_text
                );
                let js = iframe_action_js(&target.frame_path, &target.selector, action.as_str());
                page.evaluate(js.as_str()).await.map_err(Error::browser)?;
            }
        }
        TriggerKind::Scroll {
            target,
            frame_path,
            x,
            y,
        } => {
            let effective_path: Vec<String> = if frame_path.is_empty() {
                target
                    .as_ref()
                    .map(|t| t.frame_path.clone())
                    .unwrap_or_default()
            } else {
                frame_path.clone()
            };
            let js = if !effective_path.is_empty() {
                let sel = target
                    .as_ref()
                    .map(|t| t.selector.clone())
                    .ok_or_else(|| {
                        Error::invalid(
                            "trigger.scroll with frame_path requires a selector to identify the scroll container",
                        )
                    })?;
                let action = format!(
                    "el.scrollTo({{ left: {x}, top: {y}, behavior: 'instant' }});",
                    x = x,
                    y = y
                );
                iframe_action_js(&effective_path, sel.as_str(), action.as_str())
            } else if let Some(t) = target.as_ref() {
                format!(
                    "(() => {{ const el = document.querySelector({sel}); if (el) {{ el.scrollTo({{ left: {x}, top: {y}, behavior: 'instant' }}); }} else {{ throw new Error('selector not found: ' + {sel}); }} }})()",
                    sel = serde_json::to_string(&t.selector).unwrap_or_else(|_| "null".into()),
                    x = x,
                    y = y,
                )
            } else {
                format!(
                    "window.scrollTo({{ left: {x}, top: {y}, behavior: 'instant' }})",
                    x = x,
                    y = y,
                )
            };
            page.evaluate(js.as_str()).await.map_err(Error::browser)?;
        }
        TriggerKind::Evaluate { js, frame_path } => {
            if frame_path.is_empty() {
                page.evaluate(js.as_str()).await.map_err(Error::browser)?;
            } else {
                let wrapped = format!(
                    "(function(){{ const doc = (function(){{ let d = document; const path = {path}; for (const name of path) {{ const ifr = d.querySelector('iframe[name=\"' + name + '\"]') || d.querySelector('iframe#' + name); if (!ifr || !ifr.contentDocument) throw new Error('iframe not accessible: ' + name); d = ifr.contentDocument; }} return d; }})(); const w = doc.defaultView; (function(window, document){{ {body} }}).call(w, w, doc); }})()",
                    path = serde_json::to_string(frame_path).unwrap_or_else(|_| "[]".into()),
                    body = js
                );
                page.evaluate(wrapped.as_str())
                    .await
                    .map_err(Error::browser)?;
            }
        }
    }
    Ok(())
}

/// Build a `page.evaluate` expression that walks a chain of nested iframes
/// (identified by `name=` or `id=`), resolves a CSS selector inside the deepest
/// document, and runs `action` with the element bound as `el`.
fn iframe_action_js(frame_path: &[String], selector: &str, action: &str) -> String {
    let path = serde_json::to_string(frame_path).unwrap_or_else(|_| "[]".into());
    let sel = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".into());
    format!(
        "(function(){{ let doc = document; const path = {path}; for (const name of path) {{ const ifr = doc.querySelector('iframe[name=\"' + name + '\"]') || doc.querySelector('iframe#' + name); if (!ifr || !ifr.contentDocument) throw new Error('iframe not accessible: ' + name); doc = ifr.contentDocument; }} const el = doc.querySelector({sel}); if (!el) throw new Error('selector not found in iframe: ' + {sel}); {action} }})()",
        path = path,
        sel = sel,
        action = action
    )
}

/// Honor a `WaitPolicy` on a real page.
///
/// `UntilNetworkIdle` waits for the CDP `Page.lifecycleEvent` named
/// `networkAlmostIdle`. `UntilNextFrame` waits for the next
/// `requestAnimationFrame` callback to fire. Both have a soft timeout so a
/// pathological page can never hang the agent loop.
/// Union of two active-source lists by id. Sources that are active at both
/// endpoints of an unobserved interval are likely still active in the middle,
/// so the agent should treat them as "the driver(s) of motion across this
/// gap." Sources that appear in only one endpoint may have just started or
/// just ended -- still useful, but a weaker signal.
fn union_active_sources(
    a: &[crate::model::ActiveSource],
    b: &[crate::model::ActiveSource],
) -> Vec<crate::model::ActiveSource> {
    let mut out: Vec<crate::model::ActiveSource> = Vec::new();
    for s in a.iter().chain(b.iter()) {
        if !out.iter().any(|existing| existing.id == s.id) {
            out.push(s.clone());
        }
    }
    out
}

/// Pixel delta between two already-stored frames, suitable for filling the
/// `pixel_delta_hint` of an `UnobservedInterval`. Returns an error if the
/// frame artifacts cannot be read.
async fn pixel_delta_hint_between(a: &Frame, b: &Frame) -> Result<f64> {
    let (bytes_a, bytes_b) = tokio::try_join!(
        tokio::fs::read(&a.artifact_local_path),
        tokio::fs::read(&b.artifact_local_path),
    )?;
    pixel_delta_ratio(&bytes_a, &bytes_b, a.format, b.format)
}

/// Encode a sequence of RGBA frames as a real animated PNG. We drop down to
/// the `png` crate directly because `image` 0.25's PNG encoder doesn't expose
/// APNG (animation control + per-frame control chunks).
fn encode_apng(
    frames: &[image::RgbaImage],
    width: u32,
    height: u32,
    delay_ms: u16,
    out: &mut Vec<u8>,
) -> Result<()> {
    use png::{BlendOp, DisposeOp, Encoder};
    let num_frames = frames.len() as u32;
    let mut encoder = Encoder::new(std::io::Cursor::new(out), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .set_animated(num_frames, 0)
        .map_err(|e| Error::invalid(format!("apng set_animated: {e}")))?;
    encoder
        .set_frame_delay(delay_ms, 1000)
        .map_err(|e| Error::invalid(format!("apng set_frame_delay: {e}")))?;
    encoder
        .set_dispose_op(DisposeOp::None)
        .map_err(|e| Error::invalid(format!("apng set_dispose_op: {e}")))?;
    encoder
        .set_blend_op(BlendOp::Source)
        .map_err(|e| Error::invalid(format!("apng set_blend_op: {e}")))?;
    let mut writer = encoder
        .write_header()
        .map_err(|e| Error::invalid(format!("apng write_header: {e}")))?;
    for img in frames {
        writer
            .write_image_data(img.as_raw())
            .map_err(|e| Error::invalid(format!("apng write_image_data: {e}")))?;
    }
    writer
        .finish()
        .map_err(|e| Error::invalid(format!("apng finish: {e}")))?;
    Ok(())
}

async fn honor_wait_policy(page: &Page, wait: WaitPolicy) -> Result<WaitOutcome> {
    match wait {
        WaitPolicy::None => Ok(WaitOutcome::NotRequested),
        WaitPolicy::UntilNetworkIdle => wait_for_network_idle(page).await,
        WaitPolicy::UntilNextFrame => wait_for_next_frame(page).await,
    }
}

/// Wait until the page emits the `networkAlmostIdle` lifecycle event, or until
/// a soft timeout fires. Returns `Satisfied` when the event arrived and
/// `TimedOut` when the soft deadline elapsed first — the caller MUST treat
/// the two cases differently because the page state is not the same.
async fn wait_for_network_idle(page: &Page) -> Result<WaitOutcome> {
    use chromiumoxide::cdp::browser_protocol::page::EventLifecycleEvent;

    let _ = page
        .execute(
            chromiumoxide::cdp::browser_protocol::page::SetLifecycleEventsEnabledParams::builder()
                .enabled(true)
                .build()
                .map_err(|e| Error::browser(format!("build SetLifecycleEventsEnabled: {e}")))?,
        )
        .await
        .map_err(Error::browser)?;

    let mut stream = page
        .event_listener::<EventLifecycleEvent>()
        .await
        .map_err(Error::browser)?;
    let deadline = std::time::Duration::from_millis(WAIT_POLICY_TIMEOUT_MS);
    let inner = tokio::time::timeout(deadline, async {
        while let Some(ev) = stream.next().await {
            if ev.name == "networkAlmostIdle" {
                return true;
            }
        }
        false
    })
    .await;
    Ok(match inner {
        Ok(true) => WaitOutcome::Satisfied,
        _ => WaitOutcome::TimedOut,
    })
}

/// Wait for one rAF tick. Uses `evaluate` so the wait participates in the
/// page's main thread, not the agent's tokio task scheduler. Returns
/// `Satisfied` when rAF actually fired, `TimedOut` when the soft deadline
/// elapsed first (which under a paused virtual clock is the common case).
async fn wait_for_next_frame(page: &Page) -> Result<WaitOutcome> {
    let js = r#"new Promise((resolve) => requestAnimationFrame(() => resolve(true)))"#;
    let deadline = std::time::Duration::from_millis(WAIT_POLICY_TIMEOUT_MS);
    Ok(
        match tokio::time::timeout(deadline, page.evaluate(js)).await {
            Ok(Ok(_)) => WaitOutcome::Satisfied,
            _ => WaitOutcome::TimedOut,
        },
    )
}

/// Mean absolute difference of corresponding pixels, normalized to [0.0, 1.0].
/// Returns 1.0 if the two images have different sizes (treat as fully different).
fn pixel_delta_ratio(
    bytes_a: &[u8],
    bytes_b: &[u8],
    fmt_a: ImageFormat,
    fmt_b: ImageFormat,
) -> Result<f64> {
    use image::{ImageFormat as ImgFormat, ImageReader};
    use std::io::Cursor;
    let img_a = ImageReader::with_format(
        Cursor::new(bytes_a),
        match fmt_a {
            ImageFormat::Png => ImgFormat::Png,
            ImageFormat::Jpeg => ImgFormat::Jpeg,
        },
    )
    .decode()
    .map_err(|e| Error::invalid(format!("decode frame_a: {e}")))?
    .to_rgba8();
    let img_b = ImageReader::with_format(
        Cursor::new(bytes_b),
        match fmt_b {
            ImageFormat::Png => ImgFormat::Png,
            ImageFormat::Jpeg => ImgFormat::Jpeg,
        },
    )
    .decode()
    .map_err(|e| Error::invalid(format!("decode frame_b: {e}")))?
    .to_rgba8();
    if img_a.dimensions() != img_b.dimensions() {
        return Ok(1.0);
    }
    let (w, h) = img_a.dimensions();
    let count = w as u64 * h as u64 * 4;
    if count == 0 {
        return Ok(0.0);
    }
    let mut sum: u64 = 0;
    for (pa, pb) in img_a.pixels().zip(img_b.pixels()) {
        for c in 0..4 {
            sum += (pa[c] as i32 - pb[c] as i32).unsigned_abs() as u64;
        }
    }
    Ok(sum as f64 / (count as f64 * 255.0))
}

fn resolve_chrome_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("CHROME_PATH") {
        return Some(p.into());
    }
    const CANDIDATES: &[&str] = &[
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/usr/bin/google-chrome",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
    ];
    for c in CANDIDATES {
        if std::path::Path::new(c).exists() {
            return Some((*c).into());
        }
    }
    None
}

// =====================================================================
// Animation Evidence Gate (`motion.verify`)
//
// One-shot, end-to-end verification of time-dependent UI behavior. Owns its
// own session lifecycle (`launch` → episode + intent → triggers → capture
// series → contact sheet / video → assess → close). The result is the same
// `MotionVerifyReport` that the MCP tool, the CLI, the `animation-qa`
// sub-agent and CI all consume — a single contract across every entry
// point.
// =====================================================================

/// One entry on the merged trigger+capture timeline.
enum SchedEvent<'a> {
    Trigger(&'a ContractTrigger),
    Capture,
}

/// Interleave triggers and sample points onto one ascending timeline so a
/// trigger at t=200ms reliably fires BEFORE the capture at t=200ms, and a
/// sample at t=0 (before the first trigger) is captured rather than silently
/// dropped. On a shared t_ms triggers fire first (tie 0), then the capture
/// (tie 1), so the frame reflects the trigger's synchronous DOM effect.
fn build_merged_schedule<'a>(
    triggers: &'a [ContractTrigger],
    target_times_ms: &[f64],
) -> Vec<(f64, u8, SchedEvent<'a>)> {
    let mut events: Vec<(f64, u8, SchedEvent)> =
        Vec::with_capacity(triggers.len() + target_times_ms.len());
    for t in triggers {
        events.push((t.at_t_ms, 0, SchedEvent::Trigger(t)));
    }
    for &ts in target_times_ms {
        events.push((ts, 1, SchedEvent::Capture));
    }
    events.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    events
}

/// Run an Animation Evidence Gate over a single contract. Spawns a fresh
/// Chrome session, executes the contract end-to-end, writes
/// `motionlens-report.json` next to the captured frames, and returns the
/// structured report. The session is closed before returning regardless of
/// success or failure.
pub async fn run_motion_verify(contract: MotionContract) -> Result<MotionVerifyReport> {
    let launch = contract.viewport.to_launch_options(
        contract.url.clone(),
        contract.external_state.clone(),
        contract.external_state_seed,
    );
    let mut session = Session::launch(launch).await?;
    let result = run_motion_verify_inner(&mut session, &contract).await;
    let _ = session.close().await;
    result
}

async fn run_motion_verify_inner(
    session: &mut Session,
    contract: &MotionContract,
) -> Result<MotionVerifyReport> {
    if contract.sample_plan.target_times_ms.len() < 2 {
        return Err(Error::invalid(
            "sample_plan.target_times_ms must contain at least 2 entries",
        ));
    }
    // 1. Episode + intent. We also stamp `document.title` / `location.href`
    //    from whatever the URL ACTUALLY served — port collisions and
    //    redirects that hand back a different site at the same URL are
    //    otherwise mistaken for animation defects.
    let (observed_title, observed_url) = match session.observed_page_identity().await {
        Ok((t, u)) => (
            if t.is_empty() { None } else { Some(t) },
            if u.is_empty() { None } else { Some(u) },
        ),
        Err(_) => (None, None),
    };
    let (episode_id, _start_state) = session.start_episode()?;
    if let Some(intent) = contract.episode_intent.as_ref() {
        session.set_intent(intent.clone())?;
    }

    // 2. Merged schedule of triggers + sample points.
    let events = build_merged_schedule(&contract.triggers, &contract.sample_plan.target_times_ms);

    let layout_opts = if contract.sample_plan.include_layout {
        // Grade `intent.expected_targets` by real `Element.matches()` in the
        // page (any valid CSS selector), not by string-equality against the
        // tool's synthesized notation.
        let match_selectors = contract
            .episode_intent
            .as_ref()
            .map(|i| i.expected_targets.clone())
            .filter(|v| !v.is_empty());
        Some(LayoutProbeOptions {
            selectors: contract.sample_plan.focus_selectors.clone(),
            match_selectors,
            ..Default::default()
        })
    } else {
        None
    };

    let mut captured_frames: Vec<Frame> = Vec::new();
    let mut sampled_points: Vec<f64> = Vec::new();
    // Notes for sample-plan targets that the CDP-trigger paint-flush
    // (+16ms) over-shot. We capture at the current time and tell the
    // agent in `evidence_missing` so a contract that was otherwise
    // valid (e.g. trigger at_t_ms:1200 plus a sample target_t_ms:1200)
    // does not nuke the whole verify run with a backward-seek error.
    let mut sample_drift_notes: Vec<String> = Vec::new();
    for (at_t, _tie, ev) in events {
        let now = session.clock_status().await?.virtual_now_ms;
        if at_t > now {
            session.clock_advance(at_t - now, None).await?;
        } else if at_t < now {
            match &ev {
                // A Trigger going backwards is a real contract bug
                // (out-of-order `at_t_ms`) — keep failing hard.
                SchedEvent::Trigger(_) => {
                    return Err(Error::BackwardSeekUnsupported {
                        driver: "virtual-time",
                        target_ms: at_t,
                        current_ms: now,
                    });
                }
                // A Capture going backwards is almost always the
                // CDP-trigger +16ms paint-flush over-shooting the next
                // sample target. Capture at `now` (at_or_after) and
                // record the drift so the agent sees it instead of
                // dying.
                SchedEvent::Capture => {
                    sample_drift_notes.push(format!(
                        "sample_plan_drift_after_cdp_trigger: target t={:.0}ms was already \
                         past after a CDP-trigger paint-flush (now t={:.0}ms); captured at \
                         t={:.0}ms instead. Pad each sample by >=16ms after any \
                         `input_mode: \"cdp\"` trigger to hit the original target exactly.",
                        at_t, now, now
                    ));
                }
            }
        }
        match ev {
            SchedEvent::Trigger(t) => {
                session
                    .trigger(t.kind.clone(), t.wait_policy, t.input_mode)
                    .await?;
            }
            SchedEvent::Capture => {
                let f = session
                    .capture_frame(ImageFormat::Png, false, layout_opts.as_ref(), false)
                    .await?;
                sampled_points.push(f.t_ms);
                captured_frames.push(f);
            }
        }
    }

    // 3. Build the unobserved_intervals annotations the same way
    //    capture_series does, but over the merged sampled_points.
    let replay_cost = session.capabilities().replay_cost;
    let mut unobserved_intervals: Vec<UnobservedInterval> =
        Vec::with_capacity(sampled_points.len().saturating_sub(1));
    for i in 1..sampled_points.len() {
        let t0 = sampled_points[i - 1];
        let t1 = sampled_points[i];
        let span_ms = t1 - t0;
        let active_sources = union_active_sources(
            &captured_frames[i - 1].active_sources,
            &captured_frames[i].active_sources,
        );
        let pixel_delta_hint =
            pixel_delta_hint_between(&captured_frames[i - 1], &captured_frames[i])
                .await
                .ok();
        let replay_cost_hint_ms = replay_cost.replay_fixed_cost_ms
            + replay_cost.cost_per_virtual_ms * span_ms
            + replay_cost.capture_cost_ms;
        unobserved_intervals.push(UnobservedInterval {
            interval: Interval::new(t0, t1),
            span_ms,
            active_sources,
            pixel_delta_hint,
            recommended_next_t_ms: (t0 + t1) / 2.0,
            replay_cost_hint_ms,
        });
    }
    let mut series = CaptureSeriesResult {
        frames: captured_frames,
        sampled_points,
        unobserved_intervals,
    };
    let frame_ids: Vec<FrameId> = series.frames.iter().map(|f| f.frame_id.clone()).collect();

    // 4. Optional contact sheet / animated export.
    let contact_sheet = if contract.include_contact_sheet && frame_ids.len() >= 2 {
        Some(session.contact_sheet(&frame_ids, 256, None).await?)
    } else {
        None
    };
    let video = if let Some(fmt) = contract.include_video {
        if !frame_ids.is_empty() {
            Some(session.video_export(&frame_ids, fmt, 10.0).await?)
        } else {
            None
        }
    } else {
        None
    };

    // Full-page settled-state PNG — the artefact an agent would
    // otherwise reach for `browser_take_screenshot` to grab. Captured
    // AFTER every sample so the page sits at its post-animation state.
    //
    // Before the capture, prime IntersectionObserver-driven reveal
    // patterns. `captureBeyondViewport: true` renders the entire
    // document but never moves the visual viewport, so `[data-reveal]`
    // elements that wait on IO callbacks stay at `opacity:0` and the
    // fullpage shot reads as "everything below the fold is black".
    // Programmatically scroll to the document bottom, advance the
    // virtual clock to flush IO callbacks + reveal transitions, then
    // scroll back to top so the screenshot keeps the natural reading
    // order. This is the agent's expected "end state" — the page as
    // a real user would see it after they've scrolled the whole way.
    let final_fullpage_screenshot = if contract.include_final_fullpage_screenshot {
        // The fullpage shot captures `captureBeyondViewport: true`,
        // rendering the entire document. But `IntersectionObserver`
        // callbacks fire only when the visible viewport passes over an
        // element, and under a paused virtual clock + headless build
        // that signal is non-deterministic — programmatic
        // `scrollTo` / `scrollIntoView` doesn't reliably trip IO
        // before the screenshot is taken. Without priming, the shot
        // reads as "below the fold is black" on the many modern sites
        // that use `[data-reveal]`-style IO patterns.
        //
        // The fix: inject a tiny `*{opacity:1!important; transform:none!important;}`
        // override before capture so every `opacity:0` / pre-translate
        // element renders in its "fully visible" content state, then
        // remove the override right after. This shot is documented as
        // "the page's content structure visible at once" — agents who
        // need the actual scroll-driven appearance of a section read
        // the contact sheet for that section instead.
        let inject_js = "(function(){\
            try {\
                const s = document.createElement('style');\
                s.id = '__motionlens_fullpage_visible__';\
                s.textContent = '*{opacity:1!important;transform:none!important;filter:none!important;visibility:visible!important;transition:none!important;animation:none!important;}';\
                document.head.appendChild(s);\
            } catch(_) {}\
            return true;\
        })()";
        let _ = session.dom_query(inject_js).await;
        // One paint flush so the override actually paints before the
        // shot. 16ms = a single rAF tick under the virtual clock.
        session.clock_advance(32.0, None).await?;
        let bytes = screenshot(session.page_ref(), ImageFormat::Png, true).await?;
        // Remove the override so the session isn't polluted for any
        // subsequent capture / query that the agent makes against the
        // same session.
        let remove_js = "(function(){\
            try {\
                const s = document.getElementById('__motionlens_fullpage_visible__');\
                if (s) s.remove();\
            } catch(_) {}\
            return true;\
        })()";
        let _ = session.dom_query(remove_js).await;
        let stored = session
            .artifact_store_ref()
            .save(&bytes, ImageFormat::Png, false)
            .await?;
        // Decode minimally to learn width/height — we already have
        // the bytes so this is cheap.
        let (w, h) = match image::load_from_memory(&bytes) {
            Ok(img) => (img.width(), img.height()),
            Err(_) => (contract.viewport.width, contract.viewport.height),
        };
        Some(crate::model::FullpageScreenshot {
            artifact_uri: stored.uri,
            artifact_local_path: stored.path.to_string_lossy().into_owned(),
            width: w,
            height: h,
        })
    } else {
        None
    };

    // 5. Timeline sources (for coverage scoring) and quality assessment.
    let timeline_sources = session.timeline_sources().await?;
    let assessment = session.motion_assess(&frame_ids).await?;

    // 6. Evidence completeness.
    let sampled_ts: Vec<f64> = series.frames.iter().map(|f| f.t_ms).collect();
    let mut motion_sources_without_samples: Vec<String> = Vec::new();
    for a in timeline_sources
        .css_animations
        .items
        .iter()
        .chain(timeline_sources.css_transitions.items.iter())
        .chain(timeline_sources.waapi.items.iter())
    {
        if a.duration_ms <= 0.0 {
            continue;
        }
        let start = a.delay_ms.max(0.0);
        let end = start + a.duration_ms.max(0.0);
        let covered = sampled_ts.iter().any(|t| *t >= start && *t <= end);
        if !covered {
            motion_sources_without_samples
                .push(a.target_selector.clone().unwrap_or_else(|| a.id.clone()));
        }
    }
    let intent_targets_without_evidence = assessment
        .intent_match
        .as_ref()
        .map(|im| im.expected_targets_missing.clone())
        .unwrap_or_default();
    let total_motion_sources = timeline_sources.css_animations.items.len()
        + timeline_sources.css_transitions.items.len()
        + timeline_sources.waapi.items.len();
    let coverage_score = if total_motion_sources == 0 {
        // No CSS / transition / WAAPI source exists. If the page is static
        // that is a true 0. If pixels ARE moving, the motion comes from a
        // source this coverage model cannot see (rAF / canvas / WebGL /
        // Three.js): coverage over the empty coverable-source set is
        // undefined — NOT "fully covered". Returning 1.0 here would let a
        // `min_coverage_score` gate pass spuriously on a 3D page that was
        // never sampling-verified, so we report 0.0 and flag the gap in
        // `evidence_missing` below.
        0.0
    } else {
        let covered = total_motion_sources.saturating_sub(motion_sources_without_samples.len());
        covered as f64 / total_motion_sources as f64
    };
    let next_required_observation = series
        .unobserved_intervals
        .iter()
        .max_by(|a, b| {
            a.span_ms
                .partial_cmp(&b.span_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|iv| iv.recommended_next_t_ms);

    let mut evidence_missing: Vec<String> = std::mem::take(&mut sample_drift_notes);
    for sel in &intent_targets_without_evidence {
        evidence_missing.push(format!("intent_target_not_moved: {}", sel));
    }
    for src in &motion_sources_without_samples {
        evidence_missing.push(format!("motion_source_not_sampled: {}", src));
    }
    if assessment.is_static && contract.thresholds.require_non_static {
        evidence_missing.push("sequence_is_static: no per-frame pixel change detected".into());
    }
    if total_motion_sources == 0 && !assessment.is_static {
        // Pixels move but nothing is attributable to a CSS / transition /
        // WAAPI source the coverage model is defined over (rAF / canvas /
        // WebGL / Three.js). `coverage_score` is 0.0 because it is not
        // assessable, not because nothing happened — say so explicitly so
        // the agent reads it correctly and a `min_coverage_score` gate
        // does not look like a silent failure.
        let cause = if timeline_sources.raf.detected {
            "rAF detected (canvas / WebGL / Three.js?)"
        } else {
            "no rAF detected either (canvas paint / driver-side animation?)"
        };
        evidence_missing.push(format!(
            "motion_present_but_no_coverable_sources: pixels change but no \
             CSS/transition/WAAPI source exists — {cause}. coverage_score is \
             not assessable for this page; rely on smoothness / jank / \
             intent_match, or set thresholds.min_coverage_score: 0.0"
        ));
    }

    // 7. Pass/fail. The headline `passes` ANDs five gates, but they are
    //    NOT equally trustworthy — record WHICH gate failed and how much
    //    to trust it (correctness vs advisory) so a reader of the report
    //    alone tells a real defect from a known-noisy advisory metric.
    //    The most prominent field (`passes`) is the least trustworthy
    //    read in isolation.
    let thresh = &contract.thresholds;
    use crate::model::FailedGate;
    use crate::model::GateReliability::{Advisory, Correctness};
    let mut failed_gates: Vec<FailedGate> = Vec::new();
    if !assessment.is_static && assessment.smoothness < thresh.min_smoothness {
        failed_gates.push(FailedGate {
            gate: "smoothness".into(),
            reliability: Advisory,
            detail: format!(
                "smoothness {:.2} < min {:.2} — a within-sample_plan relative \
                 score; dense/uneven sampling or a count→burst sequence lowers \
                 it even on 60fps-smooth motion. Triage with the contact sheet.",
                assessment.smoothness, thresh.min_smoothness
            ),
        });
    }
    if assessment.jank_events.len() as u32 > thresh.max_jank_events {
        failed_gates.push(FailedGate {
            gate: "jank-events".into(),
            reliability: Advisory,
            detail: format!(
                "{} jank event(s) > max {} — can include pixel-cv outliers from \
                 a deliberate full-screen beat and positional-teleport from a \
                 scaleX / clip bar. Check each event's `kind` against `intent`.",
                assessment.jank_events.len(),
                thresh.max_jank_events
            ),
        });
    }
    if thresh.require_intent_match && !matches!(&assessment.intent_match, Some(im) if im.passes) {
        failed_gates.push(FailedGate {
            gate: "intent-match".into(),
            reliability: Correctness,
            detail: "intent_match.passes is not true — the declared intent \
                     (kinds / targets / duration) was not satisfied. This is \
                     a real defect, not a measurement artifact."
                .into(),
        });
    }
    // `coverage_score` measures how well the sampled timestamps cover
    // the known CSS Animation / CSS Transition / WAAPI motion sources
    // the CDP Animation domain reports. When the page is rAF / GSAP /
    // Framer Motion / Lottie driven (no CDP-known source), the model
    // has nothing to cover by construction — `coverage_score` is
    // structurally 0 and the gate would otherwise always fire. Skip
    // it in that case so agents don't have to set
    // `min_coverage_score: 0` on every JS-driven page. Emit a single
    // `evidence_missing` entry so the agent can still see the
    // constraint was skipped.
    let has_cdp_known_source = !timeline_sources.css_animations.items.is_empty()
        || !timeline_sources.css_transitions.items.is_empty()
        || !timeline_sources.waapi.items.is_empty();
    let coverage_gate_applies =
        has_cdp_known_source || coverage_score > thresh.min_coverage_score * 0.001;
    if !coverage_gate_applies {
        evidence_missing.push(
            "coverage_score 0.0 because no CSS Animation / CSS Transition / WAAPI \
             source was registered with the CDP Animation domain — the page is \
             rAF / GSAP / Framer Motion / Lottie driven. The `coverage` gate is \
             auto-skipped (structurally unmeasurable). Other gates \
             (intent-match / non-static / smoothness) still apply."
                .into(),
        );
    } else if coverage_score < thresh.min_coverage_score {
        failed_gates.push(FailedGate {
            gate: "coverage".into(),
            reliability: Advisory,
            detail: format!(
                "coverage_score {:.2} < min {:.2} — the sample plan didn't reach \
                 every CDP-registered motion source. Densify the sample_plan or \
                 set min_coverage_score lower if some sources are intentionally \
                 outside scope.",
                coverage_score, thresh.min_coverage_score
            ),
        });
    }
    if thresh.require_non_static && assessment.is_static {
        failed_gates.push(FailedGate {
            gate: "non-static".into(),
            reliability: Correctness,
            detail: "sequence is static (no per-frame pixel change) — the \
                     animation never fired or the clock never advanced. This \
                     is a real defect."
                .into(),
        });
    }
    let passes = failed_gates.is_empty();

    // 8. Confidence: weighted by coverage; penalised by every evidence gap.
    let confidence = (coverage_score / (1.0 + evidence_missing.len() as f64 * 0.5)).clamp(0.0, 1.0);

    // 9. Three-value verdict + one-line human summary so agents / CI can
    //    pattern-match without re-deriving thresholds.
    let verdict = if !passes {
        "fail"
    } else if confidence >= 0.7 {
        "pass"
    } else {
        "needs-attention"
    }
    .to_string();
    let mut verdict_human = build_verdict_human(
        &verdict,
        passes,
        &assessment,
        coverage_score,
        &evidence_missing,
    );
    // Annotate `passes:false` PER reliability. A blanket triage caveat
    // would dilute the one trustworthy signal — it talks the reader
    // OUT of a genuine intent-match failure. A correctness gate failing
    // is a real defect — say so plainly; only an advisory-only failure
    // warrants the "maybe an artifact" hedge.
    if !passes {
        let has_correctness = failed_gates
            .iter()
            .any(|g| g.reliability == crate::model::GateReliability::Correctness);
        if has_correctness {
            verdict_human.push_str(
                " — passes:false includes a CORRECTNESS gate (intent-match / non-static): \
                 the declared intent was not satisfied, or the animation never fired. This \
                 is a real defect — fix it; do NOT dismiss it as a measurement artifact \
                 (any advisory gates listed are secondary).",
            );
        } else {
            verdict_human.push_str(
                " — passes:false is from ADVISORY gates only (smoothness / jank-events / \
                 coverage): likely a measurement / choreography artifact (ambient loop, an \
                 intended full-screen beat, rAF/GSAP coverage 0), not necessarily a defect. \
                 Triage with intent_match + the contact sheet; passes:true is reliable.",
            );
        }
    }

    // Annotate the target-span sampling-noise case: when
    // duration_match is `match` but ratio is well below 1.0 (= the
    // declared duration is longer than what the target's per-target
    // settle was observed at), say so up-front. Otherwise agents see
    // `expected_ms: 1700, observed_coverage_ms: 1100, ratio: 0.65,
    // verdict: match` and pause to wonder "why is this a match?".
    if let Some(im) = &assessment.intent_match {
        if let Some(dm) = &im.duration_match {
            if dm.verdict == "match" && dm.ratio < 0.8 {
                verdict_human.push_str(&format!(
                    " — note: duration_match.ratio {:.2} (observed {:.0}ms vs \
                     expected {:.0}ms) is below 1.0 only because sample plan \
                     granularity records the target's settle_ms at the first \
                     frame to cross 0.95 progress — that frame typically \
                     lands before the declared `expected_duration_ms` ends. \
                     This is sampling noise from the target-span gate \
                     (lower bound deliberately relaxed), not a defect.",
                    dm.ratio, dm.observed_coverage_ms, dm.expected_ms,
                ));
            }
        }
    }

    let diagnosis_hints = build_diagnosis_hints(
        &assessment,
        &series.frames,
        &timeline_sources,
        contract.episode_intent.as_ref(),
        &intent_targets_without_evidence,
        &motion_sources_without_samples,
        !contract.triggers.is_empty(),
    );

    // 9b. Strip heavyweight per-frame detail from the response unless the
    //     caller opted in. Everything that consumes the captured layout —
    //     `assessment` and `contact_sheet` (both read the ledger via
    //     `find_frame`) and `diagnosis_hints` (reads `&series.frames`) —
    //     has already run. Dropping `layout_snapshot` / `thumbnail_base64`
    //     here changes neither `passes` nor any score; it only shrinks the
    //     payload the agent pays tokens for. The raw per-frame DOM is
    //     still available on demand via `frame.layout_probe` /
    //     `frame.dom_query`, and verbatim via `include_frame_detail: true`.
    //     The actual frame-strip happens after the target_selector_hint
    //     resolver below, so that resolver can still read
    //     `layout_snapshot.elements[].running_animations`.

    // Strip ambient sources from per-interval `active_sources` lists.
    // An "ambient" source is one that's active across every captured
    // unobserved interval — a marquee loop, a hero `orb-float`, a
    // brand mark rotation. They dominate the response payload (an
    // N-source × M-interval product) and offer almost no debug value
    // at the interval level. They're listed once in
    // `ambient_source_names` so the agent still knows what's running.
    //
    // include_frame_detail: true keeps the full expansion for callers
    // who need every source.
    let ambient_source_names: Vec<String> =
        if contract.include_frame_detail || series.unobserved_intervals.len() < 2 {
            Vec::new()
        } else {
            // Ambient = present in every captured interval AND its
            // `duration_ms` (one iteration × iteration count) is longer
            // than the entire sample window. That's the shape of a
            // marquee / orb-float / brand-mark spin: a long loop that
            // happens to span more than the agent's whole sample plan.
            // A one-shot 600ms `forwards`-fill entrance fails the
            // duration test even when it stays in `active_sources` for
            // every interval (CDP keeps a finished `forwards` animation
            // active), so the heuristic keeps it out of `ambient_*`.
            use std::collections::HashMap;
            let total = series.unobserved_intervals.len();
            let sample_span_ms: f64 = series
                .unobserved_intervals
                .last()
                .map(|iv| iv.interval.t1_ms)
                .unwrap_or(0.0)
                - series
                    .unobserved_intervals
                    .first()
                    .map(|iv| iv.interval.t0_ms)
                    .unwrap_or(0.0);
            let mut counts: HashMap<String, (usize, f64)> = HashMap::new();
            for iv in &series.unobserved_intervals {
                // De-duplicate within an interval first; the union of
                // both endpoints often lists the same source twice.
                let mut seen_here: std::collections::HashMap<String, f64> =
                    std::collections::HashMap::new();
                for s in &iv.active_sources {
                    let key = s.name.clone().unwrap_or_else(|| s.id.clone());
                    seen_here
                        .entry(key)
                        .and_modify(|d| *d = d.max(s.duration_ms))
                        .or_insert(s.duration_ms);
                }
                for (key, dur) in seen_here {
                    let entry = counts.entry(key).or_insert((0, 0.0));
                    entry.0 += 1;
                    entry.1 = entry.1.max(dur);
                }
            }
            let mut ambient: Vec<String> = counts
                .into_iter()
                .filter_map(|(k, (count, max_dur))| {
                    // The duration threshold has a safety margin: a
                    // source whose nominal duration equals the sample
                    // window (e.g. 600ms loop sampled across 0..600ms)
                    // shouldn't get classified as ambient just because
                    // of rounding. Require strictly longer.
                    if count == total && max_dur > sample_span_ms * 1.05 {
                        Some(k)
                    } else {
                        None
                    }
                })
                .collect();
            ambient.sort();
            ambient
        };
    if !ambient_source_names.is_empty() {
        let ambient_set: std::collections::HashSet<&str> =
            ambient_source_names.iter().map(String::as_str).collect();
        for iv in &mut series.unobserved_intervals {
            iv.active_sources.retain(|s| {
                let key = s.name.as_deref().unwrap_or(s.id.as_str());
                !ambient_set.contains(key)
            });
        }
    }

    // Resolve `target_selector_hint` for any remaining active_sources
    // whose `target_selector` is hashed (CDP `cssId` on an element
    // without a stable id). Cross-reference `name` against every
    // captured layout snapshot's `elements[].running_animations` and,
    // when exactly one element runs the matching animation, pin its
    // selector into `target_selector_hint`. Skip the resolution for
    // names matched by multiple elements (a stagger) — there's no
    // single right answer and the raw cross-reference data is still
    // available in the snapshots.
    {
        use std::collections::HashMap;
        // name -> set of selectors running it (across all captured
        // layout snapshots, deduplicated). Use BTreeSet so the
        // chosen "single" hint is deterministic.
        let mut name_to_selectors: HashMap<String, std::collections::BTreeSet<String>> =
            HashMap::new();
        for f in &series.frames {
            if let Some(ls) = f.layout_snapshot.as_ref() {
                for e in &ls.elements {
                    for anim in &e.running_animations {
                        name_to_selectors
                            .entry(anim.clone())
                            .or_default()
                            .insert(e.selector.clone());
                    }
                }
            }
        }
        let pick_hint = |source_name: Option<&str>| -> Option<String> {
            let n = source_name?;
            let set = name_to_selectors.get(n)?;
            if set.len() == 1 {
                set.iter().next().cloned()
            } else {
                None
            }
        };
        for f in &mut series.frames {
            for s in &mut f.active_sources {
                if s.target_selector_hint.is_none() {
                    s.target_selector_hint = pick_hint(s.name.as_deref());
                }
            }
        }
        for iv in &mut series.unobserved_intervals {
            for s in &mut iv.active_sources {
                if s.target_selector_hint.is_none() {
                    s.target_selector_hint = pick_hint(s.name.as_deref());
                }
            }
        }
    }

    // NOW strip per-frame `layout_snapshot` / `thumbnail_base64`
    // (moved from above) so the hint resolver above still has access
    // to `running_animations` from the layout snapshots.
    if !contract.include_frame_detail {
        for f in &mut series.frames {
            f.layout_snapshot = None;
            f.thumbnail_base64 = None;
        }
    }

    // 10. Persist the report. The canonical copy lives in the session's
    //     artifact store (always writable, session-scoped, next to the
    //     captured frames) and is what `artifact_local_path` points at.
    //     A best-effort copy is also placed in the cwd so `gate-check`
    //     and CI find `./motionlens-report.json` without negotiating
    //     paths; that copy's success is reported back via
    //     `cwd_report_path`.
    let report_path_store = session.artifact_store_root().join("motionlens-report.json");
    let report_path_cwd = std::env::current_dir()
        .ok()
        .map(|p| p.join("motionlens-report.json"));
    let session_id = session.id.clone();
    // We serialize the report twice: first with an empty cwd_report_path so
    // we can compute the canonical JSON, then again after the cwd write
    // settles so the cwd_report_path field reflects reality. The second
    // pass is unconditional — even when the cwd write fails the report
    // still ends up in the artifact store with cwd_report_path = None.
    let mut report = MotionVerifyReport {
        contract_url: contract.url.clone(),
        contract_viewport: contract.viewport.clone(),
        observed_title: observed_title.clone(),
        observed_url: observed_url.clone(),
        passes,
        verdict,
        verdict_human,
        failed_gates,
        confidence,
        session_id,
        episode_id,
        assessment,
        contact_sheet,
        video,
        final_fullpage_screenshot,
        evidence_missing,
        coverage_score,
        motion_sources_without_samples,
        intent_targets_without_evidence,
        next_required_observation,
        timeline_sources,
        frames: series.frames,
        unobserved_intervals: series.unobserved_intervals,
        ambient_source_names,
        artifact_local_path: report_path_store.to_string_lossy().into_owned(),
        cwd_report_path: None,
        diagnosis_hints,
    };
    if let Some(cwd_path) = report_path_cwd.as_ref() {
        let preview_json = serde_json::to_string_pretty(&report)
            .map_err(|e| Error::invalid(format!("serialize motion verify report: {e}")))?;
        if tokio::fs::write(cwd_path, &preview_json).await.is_ok() {
            report.cwd_report_path = Some(cwd_path.to_string_lossy().into_owned());
        }
    }
    let report_json = serde_json::to_string_pretty(&report)
        .map_err(|e| Error::invalid(format!("serialize motion verify report: {e}")))?;
    tokio::fs::write(&report_path_store, &report_json).await?;
    // If the cwd copy landed, refresh it so its cwd_report_path field
    // matches the canonical store copy. We only re-write when the first
    // attempt succeeded, so a read-only cwd still won't fail the verify.
    if let Some(cwd_path) = report.cwd_report_path.as_ref() {
        let _ = tokio::fs::write(cwd_path, &report_json).await;
    }

    // The full report (every frame's layout_snapshot) is now persisted to
    // disk at `artifact_local_path`. The value returned over MCP must fit
    // Claude Code's MCP tool-result ceiling — default 25,000 tokens (it
    // warns at 10k); past it the result is dropped / force-persisted and
    // the agent gets a file-shuffle detour instead of an answer (an
    // ordinary 17-sample contract can hit ~50k chars and hard-error).
    // `MCP_SAFE_CHARS` is set from two measured points — 36,640 chars
    // passed, 50,627 chars hard-errored — conservatively below the failure
    // point (report JSON tokenizes densely, so chars/token is low).
    // Degrade progressively; the disk copies above are untouched so
    // nothing is lost — the agent Reads `artifact_local_path` for detail.
    const MCP_SAFE_CHARS: usize = 35_000;
    let measure = |r: &MotionVerifyReport| {
        serde_json::to_string(r)
            .map(|s| s.len())
            .unwrap_or(usize::MAX)
    };
    if measure(&report) > MCP_SAFE_CHARS {
        let disk = report.artifact_local_path.clone();
        // 1) drop per-frame heavyweight detail
        for f in &mut report.frames {
            f.layout_snapshot = None;
            f.thumbnail_base64 = None;
        }
        let mut dropped = "per-frame layout_snapshot / thumbnail";
        // 2) still too big -> drop the frames array entirely
        if measure(&report) > MCP_SAFE_CHARS {
            report.frames.clear();
            dropped = "all per-frame data";
            // 3) still too big -> drop unobserved-interval annotations
            //    (their active_sources lists dominate on many-element
            //    pages; next_required_observation stays on the root)
            if measure(&report) > MCP_SAFE_CHARS {
                report.unobserved_intervals.clear();
                dropped = "all per-frame data and unobserved_intervals";
            }
        }
        report.evidence_missing.push(format!(
            "response_truncated_for_mcp_limit: {dropped} were dropped from the \
             RETURNED payload to fit the MCP result ceiling (~25k tokens). The \
             COMPLETE report is on disk at artifact_local_path ({disk}) — Read \
             that file; nothing is lost (passes / verdict / assessment / \
             coverage / contact_sheet are still inline here)."
        ));
    }

    Ok(report)
}

/// `motion.suggest_intent` — fire a short capture series at `url`
/// (optionally after a trigger), classify what moved, and return an
/// `EpisodeIntent` draft.  Closes the session before returning regardless
/// of success or failure.  The result is advisory; the agent is expected
/// to refine the description / drop targets it doesn't own / tighten
/// `forbidden_kinds` before pasting into a final `MotionContract`.
pub async fn run_motion_suggest_intent(
    req: MotionSuggestIntentRequest,
) -> Result<MotionSuggestIntentResponse> {
    let launch = req.viewport.to_launch_options(req.url.clone(), None, 0);
    let mut session = Session::launch(launch).await?;
    let result = run_motion_suggest_intent_inner(&mut session, &req).await;
    let _ = session.close().await;
    result
}

async fn run_motion_suggest_intent_inner(
    session: &mut Session,
    req: &MotionSuggestIntentRequest,
) -> Result<MotionSuggestIntentResponse> {
    let window_ms = req.probe_window_ms.unwrap_or(480.0).max(40.0);
    let steps = req.probe_steps.unwrap_or(6).max(2) as usize;
    let target_times_ms: Vec<f64> = (0..steps)
        .map(|i| window_ms * (i as f64) / ((steps - 1) as f64))
        .collect();

    let (_episode_id, _start_state) = session.start_episode()?;

    let events = build_merged_schedule(&req.triggers, &target_times_ms);

    // `focus_selectors` plays the same observer-named role here that
    // `expected_targets` plays in `motion.verify` — the agent is saying
    // "these are the elements I want classified". Mirror the bypass:
    // pass them as `match_selectors` so the layout probe's visibility
    // filter doesn't drop an opacity:0 hero entrance from the t=0
    // sample and make the entire window look static. Without this,
    // suggest_intent answers `is_static: true` for the canonical
    // hero-entrance pattern when the agent narrowed focus to it.
    let layout_opts = Some(LayoutProbeOptions {
        selectors: req.focus_selectors.clone(),
        match_selectors: req.focus_selectors.clone(),
        ..Default::default()
    });

    let mut captured_frames: Vec<Frame> = Vec::new();
    for (at_t, _tie, ev) in events {
        let now = session.clock_status().await?.virtual_now_ms;
        if at_t > now {
            session.clock_advance(at_t - now, None).await?;
        } else if at_t < now {
            return Err(Error::BackwardSeekUnsupported {
                driver: "virtual-time",
                target_ms: at_t,
                current_ms: now,
            });
        }
        match ev {
            SchedEvent::Trigger(t) => {
                session
                    .trigger(t.kind.clone(), t.wait_policy, t.input_mode)
                    .await?;
            }
            SchedEvent::Capture => {
                let f = session
                    .capture_frame(ImageFormat::Png, false, layout_opts.as_ref(), false)
                    .await?;
                captured_frames.push(f);
            }
        }
    }

    let frame_ids: Vec<FrameId> = captured_frames.iter().map(|f| f.frame_id.clone()).collect();
    let assessment = session.motion_assess(&frame_ids).await?;

    // When the page is static (no per-frame pixel change above the
    // NO_MOTION_FLOOR), motion.assess can still emit `moved_selectors`
    // from layout snapshot drift — bbox rounding, font measuring, etc.
    // Those are not meaningful motion targets and including them in
    // `expected_targets` would mislead the agent.  Drop them here so the
    // draft stays consistent: `is_static=true` → empty kinds + targets.
    let (motion_kinds, moved_selectors) = if assessment.is_static {
        (Vec::new(), Vec::new())
    } else {
        (
            assessment.detected_motion_kinds.clone(),
            assessment.moved_selectors.clone(),
        )
    };

    // Estimate effective duration: the t_ms of the last frame whose
    // preceding pair carried a delta > mean/4. When the motion never
    // settles inside the probe window the suggestion defers to the full
    // coverage_ms rather than guessing.
    let suggested_duration_ms: Option<f64> =
        if assessment.is_static || assessment.per_transition_delta.is_empty() {
            None
        } else {
            let threshold = assessment.mean_delta.abs() / 4.0;
            let mut last_active: Option<usize> = None;
            for (i, d) in assessment.per_transition_delta.iter().enumerate() {
                if d.abs() > threshold {
                    last_active = Some(i);
                }
            }
            last_active.and_then(|i| captured_frames.get(i + 1).map(|f| f.t_ms))
        };

    let mut notes: Vec<String> = Vec::new();
    if assessment.is_static {
        notes.push(format!(
            "No per-frame pixel change inside the {:.0}ms probe window. \
             The animation may depend on a trigger / scroll / longer duration \
             than probed. Re-run with explicit triggers, a larger probe_window_ms, \
             or `motion.audit_required` for a static check.",
            window_ms
        ));
    }
    if moved_selectors.is_empty() && !assessment.is_static {
        notes.push(
            "Pixels changed but no stable selector was identified — the layout \
             probe found no element whose bbox / opacity / transform shifted. \
             Either the animation is on an unsemantic element (canvas / SVG) or \
             `focus_selectors` excluded it."
                .into(),
        );
    }
    if let Some(d) = suggested_duration_ms {
        notes.push(format!(
            "Animation appears to settle around {:.0}ms — copied into \
             `suggested_intent.expected_duration_ms`.",
            d
        ));
    }
    if assessment.smoothness < 0.6 && !assessment.is_static {
        notes.push(format!(
            "Smoothness over the probe is {:.2} (poor/acceptable). The probe \
             sampling may be too coarse; re-run with more `probe_steps`.",
            assessment.smoothness
        ));
    }

    let description = if assessment.is_static {
        format!(
            "No motion observed at {} within {:.0}ms.",
            req.url, assessment.coverage_ms
        )
    } else {
        let kinds_text = if motion_kinds.is_empty() {
            "pixel-level motion".to_string()
        } else {
            motion_kinds
                .iter()
                .map(motion_category_label)
                .collect::<Vec<_>>()
                .join(" + ")
        };
        let dur = suggested_duration_ms.unwrap_or(assessment.coverage_ms);
        format!(
            "Observed motion at {}: {} over ~{:.0}ms.",
            req.url, kinds_text, dur
        )
    };

    let suggested_intent = EpisodeIntent {
        description,
        expected_duration_ms: suggested_duration_ms,
        expected_progress_range: None,
        expected_kinds: motion_kinds.clone(),
        expected_targets: moved_selectors.clone(),
        forbidden_kinds: Vec::new(),
        expected_easing: None,
    };

    // Identical to motion.verify's port-collision early warning.
    // Reads after the probe finishes so the same Runtime context is
    // still live; falls back to None on any CDP hiccup.
    let (observed_title, observed_url) = match session.observed_page_identity().await {
        Ok((t, u)) => (Some(t), Some(u)),
        Err(_) => (None, None),
    };

    Ok(MotionSuggestIntentResponse {
        suggested_intent,
        observed_coverage_ms: assessment.coverage_ms,
        detected_motion_kinds: motion_kinds,
        moved_selectors,
        smoothness: assessment.smoothness,
        is_static: assessment.is_static,
        notes,
        observed_title,
        observed_url,
    })
}

/// Per-selector easing curve fit + overshoot detection + multi-target
/// stagger uniformity.  Returns three parallel artifacts populated into
/// `AnimationAssessment`. Pure function — no CDP calls, no IO.
///
/// Per axis the most-moving signal (`translate-x`, `translate-y`,
/// `opacity`) is picked automatically.  The observed curve is normalised
/// to `[0..1]` and compared by RMS against a fixed easing catalog
/// (`linear / ease-in / ease-out / ease-in-out / ease-in-cubic /
/// ease-out-cubic / ease-in-out-cubic`).  Curves with fewer than three
/// samples or no measurable motion produce no entry.
fn compute_easing_stagger_overshoot(
    frames: &[&Frame],
    moved_selectors: &[String],
) -> (
    Vec<crate::model::TargetEasing>,
    Vec<crate::model::OvershootEvent>,
    Option<f64>,
) {
    use crate::model::{OvershootEvent, TargetEasing};

    if frames.len() < 3 {
        return (Vec::new(), Vec::new(), None);
    }
    let t0 = frames[0].t_ms;
    let t_end = frames[frames.len() - 1].t_ms;
    let total_span = (t_end - t0).max(1e-9);

    let mut per_target_easing: Vec<TargetEasing> = Vec::new();
    let mut overshoots: Vec<OvershootEvent> = Vec::new();
    let mut onsets: Vec<f64> = Vec::new();

    for sel in moved_selectors {
        // We track several candidate signals per selector and pick the
        // one with the largest peak-to-trough magnitude as the dominant
        // axis. Using the bbox **center** (not the top-left corner)
        // cancels the bbox-widening artefact a 90° rotation introduces
        // on a square, so a `rotate(360deg)` keyframe doesn't make a
        // linear translate look ease-in-out.
        let mut center_x: Vec<Option<f64>> = Vec::with_capacity(frames.len());
        let mut center_y: Vec<Option<f64>> = Vec::with_capacity(frames.len());
        let mut op_series: Vec<Option<f64>> = Vec::with_capacity(frames.len());
        // Pure translate decoded from the CSS `transform` string, used
        // when the element animates via `transform: translate*` rather
        // than positional `left` / `top` / margins.
        let mut tx_series: Vec<Option<f64>> = Vec::with_capacity(frames.len());
        let mut ty_series: Vec<Option<f64>> = Vec::with_capacity(frames.len());
        for f in frames {
            let ls = f.layout_snapshot.as_ref();
            // bbox is viewport-relative; add the page's scroll offset
            // to recover a document-relative position. Without this,
            // a single `trigger.scroll` of e.g. 900px makes every
            // visible element's bbox-centre jump by 900px and the
            // overshoot detector lights up every element with a
            // bogus `peak_progress >> 1`.
            let scroll_x = ls.map(|s| s.scroll_x).unwrap_or(0.0);
            let scroll_y = ls.map(|s| s.scroll_y).unwrap_or(0.0);
            let probe = ls.and_then(|s| s.elements.iter().find(|e| &e.selector == sel));
            center_x.push(probe.map(|p| p.bbox[0] + p.bbox[2] / 2.0 + scroll_x));
            center_y.push(probe.map(|p| p.bbox[1] + p.bbox[3] / 2.0 + scroll_y));
            op_series.push(probe.map(|p| p.opacity));
            let translate = probe
                .and_then(|p| p.transform.as_deref())
                .and_then(parse_translate_from_css_transform);
            tx_series.push(translate.map(|(x, _)| x));
            ty_series.push(translate.map(|(_, y)| y));
        }
        let candidates: [(&str, &Vec<Option<f64>>); 5] = [
            ("transform-translate-x", &tx_series),
            ("transform-translate-y", &ty_series),
            ("translate-x", &center_x),
            ("translate-y", &center_y),
            ("opacity", &op_series),
        ];
        let (axis, raw_values) = match candidates
            .into_iter()
            .filter_map(|(name, series)| {
                let vals: Vec<f64> = series.iter().filter_map(|v| *v).collect();
                if vals.len() < 3 {
                    return None;
                }
                let lo = vals.iter().cloned().fold(f64::INFINITY, f64::min);
                let hi = vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                let mag = hi - lo;
                if mag <= 1e-3 {
                    return None;
                }
                Some((mag, name, series))
            })
            .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        {
            Some((_, name, series)) => {
                let vals: Vec<f64> = series.iter().filter_map(|v| *v).collect();
                (name, vals)
            }
            None => continue,
        };
        // Times aligned with raw_values entries: filter frames where the probe was present.
        let times: Vec<f64> = frames
            .iter()
            .filter_map(|f| {
                let present = f
                    .layout_snapshot
                    .as_ref()
                    .is_some_and(|s| s.elements.iter().any(|e| &e.selector == sel));
                if present {
                    Some(f.t_ms)
                } else {
                    None
                }
            })
            .collect();
        if times.len() != raw_values.len() || times.len() < 3 {
            continue;
        }
        let start = raw_values[0];
        let end = *raw_values.last().unwrap();
        let span = end - start;
        if span.abs() < 1e-3 {
            continue;
        }
        let progress: Vec<f64> = raw_values.iter().map(|v| (v - start) / span).collect();
        let norm_times: Vec<f64> = times.iter().map(|t| (t - t0) / total_span).collect();

        type EasingTemplate = (&'static str, fn(f64) -> f64);
        let templates: &[EasingTemplate] = &[
            ("linear", easing_linear),
            ("ease-in", easing_in),
            ("ease-out", easing_out),
            ("ease-in-out", easing_in_out),
            ("ease-in-cubic", easing_in_cubic),
            ("ease-out-cubic", easing_out_cubic),
            ("ease-in-out-cubic", easing_in_out_cubic),
        ];
        let (best_name, best_rms) = templates
            .iter()
            .map(|(name, f)| {
                let rms = (norm_times
                    .iter()
                    .zip(&progress)
                    .map(|(nt, p)| {
                        let expected = f(*nt);
                        (expected - p).powi(2)
                    })
                    .sum::<f64>()
                    / (progress.len() as f64))
                    .sqrt();
                (*name, rms)
            })
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap();

        let max_progress = progress.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        if max_progress > 1.05 {
            // Peak time: first index reaching the peak.
            let peak_t = progress
                .iter()
                .position(|p| (*p - max_progress).abs() < 1e-6)
                .map(|i| times[i])
                .unwrap_or(*times.last().unwrap());
            overshoots.push(OvershootEvent {
                selector: sel.clone(),
                axis: axis.to_string(),
                peak_progress: max_progress,
                peak_t_ms: peak_t,
            });
        }

        // Onset: first transition where progress crossed 5%.
        let onset_t = progress
            .iter()
            .position(|p| p.abs() > 0.05)
            .map(|i| if i == 0 { times[0] } else { times[i] })
            .unwrap_or(times[0]);
        onsets.push(onset_t);

        // Settle: the earliest sample after which progress stays at
        // 95%+ until the end of the sequence. `None` when the target
        // never reached 95% inside the sample window (the motion is
        // still in flight or the curve has a low-amplitude tail).
        // Used by intent_match.duration_match to measure the
        // observed span against the expected_targets only, so ambient
        // long-running animations do not inflate the verdict.
        let settle_t = {
            let mut last_idx: Option<usize> = None;
            for (i, p) in progress.iter().enumerate() {
                if *p >= 0.95 {
                    last_idx = Some(i);
                    // Check the rest of the sequence stays >= 0.95.
                    let stays = progress[i..].iter().all(|q| *q >= 0.95);
                    if stays {
                        break;
                    } else {
                        last_idx = None;
                    }
                }
            }
            last_idx.map(|i| times[i])
        };

        per_target_easing.push(TargetEasing {
            selector: sel.clone(),
            axis: axis.to_string(),
            observed_values: raw_values,
            observed_progress: progress,
            best_match_easing: best_name.to_string(),
            rms_error: best_rms,
            onset_ms: Some(onset_t),
            settle_ms: settle_t,
        });
    }

    let stagger_uniformity = if onsets.len() >= 2 {
        let mut sorted = onsets.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let gaps: Vec<f64> = sorted.windows(2).map(|w| w[1] - w[0]).collect();
        if gaps.is_empty() {
            None
        } else {
            let mean = gaps.iter().sum::<f64>() / (gaps.len() as f64);
            if mean.abs() < 1e-6 {
                // All onsets identical — perfectly synchronised, which is the
                // limiting case of "uniform stagger of zero ms".
                Some(1.0)
            } else {
                let variance =
                    gaps.iter().map(|g| (g - mean).powi(2)).sum::<f64>() / (gaps.len() as f64);
                let stddev = variance.sqrt();
                let cv = stddev / mean;
                Some((1.0 / (1.0 + cv)).clamp(0.0, 1.0))
            }
        }
    } else {
        None
    };

    (per_target_easing, overshoots, stagger_uniformity)
}

/// Parse a CSS `transform` computed-style string and return the
/// translation component `(tx, ty)` in CSS pixels. Returns `None` for
/// `"none"` or values that don't include a translate term.  Supports
/// `matrix(a, b, c, d, tx, ty)`, `matrix3d(a..p)` (uses entries 12, 13),
/// `translate(x[, y])`, `translateX(x)`, `translateY(y)`, and
/// `translate3d(x, y, z)`. Other forms (`scale`, `rotate` alone) decode
/// to `(0.0, 0.0)`.
fn parse_translate_from_css_transform(s: &str) -> Option<(f64, f64)> {
    let s = s.trim();
    if s.is_empty() || s == "none" {
        return None;
    }
    fn parse_px(token: &str) -> Option<f64> {
        let t = token.trim().trim_end_matches("px");
        t.parse::<f64>().ok()
    }
    if let Some(rest) = s.strip_prefix("matrix(").and_then(|r| r.strip_suffix(')')) {
        let parts: Vec<&str> = rest.split(',').collect();
        if parts.len() == 6 {
            let tx = parse_px(parts[4])?;
            let ty = parse_px(parts[5])?;
            return Some((tx, ty));
        }
    }
    if let Some(rest) = s
        .strip_prefix("matrix3d(")
        .and_then(|r| r.strip_suffix(')'))
    {
        let parts: Vec<&str> = rest.split(',').collect();
        if parts.len() == 16 {
            let tx = parse_px(parts[12])?;
            let ty = parse_px(parts[13])?;
            return Some((tx, ty));
        }
    }
    if let Some(rest) = s
        .strip_prefix("translate(")
        .and_then(|r| r.strip_suffix(')'))
    {
        let parts: Vec<&str> = rest.split(',').collect();
        let tx = parse_px(parts[0])?;
        let ty = parts.get(1).and_then(|p| parse_px(p)).unwrap_or(0.0);
        return Some((tx, ty));
    }
    if let Some(rest) = s
        .strip_prefix("translateX(")
        .and_then(|r| r.strip_suffix(')'))
    {
        return Some((parse_px(rest)?, 0.0));
    }
    if let Some(rest) = s
        .strip_prefix("translateY(")
        .and_then(|r| r.strip_suffix(')'))
    {
        return Some((0.0, parse_px(rest)?));
    }
    if let Some(rest) = s
        .strip_prefix("translate3d(")
        .and_then(|r| r.strip_suffix(')'))
    {
        let parts: Vec<&str> = rest.split(',').collect();
        if parts.len() >= 2 {
            let tx = parse_px(parts[0])?;
            let ty = parse_px(parts[1])?;
            return Some((tx, ty));
        }
    }
    None
}

fn easing_linear(t: f64) -> f64 {
    t
}
fn easing_in(t: f64) -> f64 {
    // Quadratic ease-in.
    t * t
}
fn easing_out(t: f64) -> f64 {
    // Quadratic ease-out.
    1.0 - (1.0 - t).powi(2)
}
fn easing_in_out(t: f64) -> f64 {
    if t < 0.5 {
        2.0 * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
    }
}
fn easing_in_cubic(t: f64) -> f64 {
    t * t * t
}
fn easing_out_cubic(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}
fn easing_in_out_cubic(t: f64) -> f64 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

fn easing_hint_label(h: crate::model::EasingHint) -> &'static str {
    use crate::model::EasingHint;
    match h {
        EasingHint::Linear => "linear",
        EasingHint::Ease => "ease-in-out",
        EasingHint::EaseIn => "ease-in",
        EasingHint::EaseOut => "ease-out",
        EasingHint::EaseInOut => "ease-in-out",
        EasingHint::EaseInCubic => "ease-in-cubic",
        EasingHint::EaseOutCubic => "ease-out-cubic",
        EasingHint::EaseInOutCubic => "ease-in-out-cubic",
    }
}

/// Build `DiagnosisHint` candidates for a finished `motion.verify` run.
///
/// The function is heuristic: each branch maps an observable failure
/// signature to a specific root-cause candidate, paired with the exact
/// next probe the agent should run to confirm.  Hints are additive (a
/// single failure can produce several candidates) — the agent picks
/// which one to investigate next based on `suggested_probe`.
fn build_diagnosis_hints(
    assessment: &AnimationAssessment,
    frames: &[Frame],
    timeline_sources: &TimelineSources,
    intent: Option<&EpisodeIntent>,
    intent_targets_without_evidence: &[String],
    motion_sources_without_samples: &[String],
    had_triggers: bool,
) -> Vec<DiagnosisHint> {
    use serde_json::json;
    let mut hints: Vec<DiagnosisHint> = Vec::new();

    let last_layout: Option<&LayoutSnapshot> =
        frames.iter().rev().find_map(|f| f.layout_snapshot.as_ref());
    let first_layout: Option<&LayoutSnapshot> =
        frames.iter().find_map(|f| f.layout_snapshot.as_ref());

    // --- expected-targets-all-missing: every declared expected_target
    //     is absent from every captured layout. Listed BEFORE the
    //     per-selector dives so a reader sees the meta-cause first —
    //     a port collision serving a different site at the same URL
    //     would otherwise surface as N individual selector-not-found
    //     hints that obscure the real cause. This meta-hint surfaces
    //     "the URL is probably wrong / the page never reached / a
    //     login wall sits in front" as a single up-front candidate. ---
    if let Some(intent_e) = intent {
        let exp = &intent_e.expected_targets;
        if !exp.is_empty() {
            let present_anywhere = |sel: &str| -> bool {
                let in_layout = |ls: Option<&LayoutSnapshot>| -> bool {
                    ls.is_some_and(|l| l.elements.iter().any(|e| e.selector == sel))
                };
                in_layout(first_layout) || in_layout(last_layout)
            };
            let all_missing = exp.iter().all(|s| !present_anywhere(s.as_str()));
            // Suppress when intent_match has actually seen at least one
            // expected_target moving — layout_snapshot.elements only
            // carries elements probed at sample time, but a selector
            // that animated via per_target_easing / moved_selectors
            // counts as observed even when it isn't in that snapshot.
            // Without this suppression the meta hint co-appears with
            // `intent_match.passes: true`, a noisy false-positive that
            // confuses the verdict reader.
            let observed_seen = assessment
                .intent_match
                .as_ref()
                .is_some_and(|im| !im.expected_targets_seen.is_empty());
            if all_missing && !observed_seen {
                hints.push(DiagnosisHint {
                    code: "expected-targets-all-missing".into(),
                    target_selector: None,
                    message: format!(
                        "ALL {} declared `expected_targets` ({:?}) are absent from every \
                         captured layout snapshot. Before chasing per-selector causes, \
                         **rule out a page-level meta-cause first** — these are the most \
                         common reasons every target disappears at once: (a) the URL is \
                         serving unexpected content (a dev port collision, a redirect, \
                         a wrong build), (b) the page never reached the intended state \
                         (network failure, an auth / login wall in front, JS error at \
                         load), (c) contract drift (the selectors were renamed in a \
                         refactor and the contract was not updated). Confirm the page \
                         actually rendered what you expect before treating per-selector \
                         hints as the diagnosis.",
                        exp.len(),
                        exp
                    ),
                    observed: json!({
                        "expected_targets": exp,
                        "all_missing_from_layout": true,
                    }),
                    suggested_probe: Some(
                        "frame.dom_query { js: 'JSON.stringify({ title: document.title, href: location.href })' } \
                         — confirm the URL and title match what your contract expects before drilling individual selectors."
                            .to_string(),
                    ),
                });
            }
        }
    }

    // --- viewport-covered-by-overlay: a fullscreen overlay (loader /
    //     splash / modal backdrop) covers the entire viewport across
    //     every captured frame, so the contact_sheet looks "stuck on
    //     the loader" even though expected_targets may be animating
    //     behind it. Tell the agent to hide the overlay with an
    //     evaluate trigger before sampling. Without this hint an agent
    //     gives up on motion.verify and falls back to a Playwright
    //     screenshot to confirm visually — a direct violation of the
    //     skill's "no single-frame Playwright shot" rule. ---
    {
        use std::collections::HashMap;
        let viewport_w = first_layout.map(|s| s.viewport_width).unwrap_or(0.0);
        let viewport_h = first_layout.map(|s| s.viewport_height).unwrap_or(0.0);
        let vw_area = viewport_w * viewport_h;
        let frame_count = frames
            .iter()
            .filter(|f| f.layout_snapshot.is_some())
            .count();
        if vw_area > 0.0 && frame_count > 1 {
            let mut overlay_counts: HashMap<&str, usize> = HashMap::new();
            let mut overlay_sample_bbox: HashMap<&str, [f64; 4]> = HashMap::new();
            for f in frames {
                if let Some(ls) = f.layout_snapshot.as_ref() {
                    for e in &ls.elements {
                        // Skip document-root / page-shell containers
                        // that are intentionally viewport-sized; they
                        // aren't overlays. Same exclusion silent-
                        // static-prominent-element already uses.
                        let tag = e.tag.as_str();
                        if matches!(tag, "html" | "body" | "main") {
                            continue;
                        }
                        let area = e.bbox[2] * e.bbox[3];
                        if area / vw_area >= 0.85 {
                            *overlay_counts.entry(e.selector.as_str()).or_insert(0) += 1;
                            overlay_sample_bbox
                                .entry(e.selector.as_str())
                                .or_insert(e.bbox);
                        }
                    }
                }
            }
            // Fire whenever a non-shell element covers the viewport
            // across every snapshotted frame. intent_match can be
            // fully green at this point (layout probe sees behind the
            // overlay even when pixel CV doesn't), so the hint is
            // primarily informational: "your contact_sheet looks
            // stuck on this overlay — hide it with an evaluate
            // trigger and you'll see the behind-overlay animation in
            // the screenshot too". An agent can have a green verify
            // and still fall back to Playwright to confirm visually
            // because the contact_sheet is opaque.
            for (sel, cnt) in overlay_counts.iter() {
                if *cnt < frame_count {
                    continue;
                }
                let bbox = overlay_sample_bbox.get(*sel).copied().unwrap_or([0.0; 4]);
                let coverage = (bbox[2] * bbox[3]) / vw_area;
                hints.push(DiagnosisHint {
                    code: "viewport-covered-by-overlay".into(),
                    target_selector: Some(sel.to_string()),
                    message: format!(
                        "`{sel}` covers {pct:.0}% of the viewport across every \
                         captured frame. The layout probe still sees behind it \
                         (so `intent_match` may already be green), but the \
                         `contact_sheet` PNG only shows this overlay — easy to \
                         misread as \"the page is broken\". Hide it with an \
                         evaluate trigger at `at_t_ms: 0` so the contact sheet \
                         and the screenshot-driven gates see the real page. \
                         Do NOT reach for Playwright to \"see what's behind\" \
                         — this hint exists precisely so you don't have to.",
                        sel = sel,
                        pct = coverage * 100.0,
                    ),
                    observed: json!({
                        "covering_selector": sel,
                        "coverage_fraction": coverage,
                        "covered_in_every_frame": true,
                    }),
                    suggested_probe: Some(format!(
                        "Add to `triggers` in the contract: \
                         {{ \"at_t_ms\": 0, \"kind\": {{ \"kind\": \"evaluate\", \
                         \"js\": \"document.querySelector('{sel}').style.display = 'none'\" }} }} \
                         — then re-run motion.verify.",
                        sel = sel,
                    )),
                });
            }
        }
    }

    // --- intent_targets_without_evidence: per-selector deep dive ---
    for sel in intent_targets_without_evidence {
        let first_probe = first_layout.and_then(|l| l.elements.iter().find(|e| &e.selector == sel));
        let last_probe = last_layout.and_then(|l| l.elements.iter().find(|e| &e.selector == sel));

        // intent-target-parent-of-moving-element: the declared
        // selector exists, but its own bbox / opacity / transform is
        // static (= not in moved_selectors), AND at least one moving
        // element across any frame has `sel` in its
        // `ancestor_match_selectors`. The agent declared the parent
        // while the animation runs on the children — name the
        // descendants and stop the user from chasing raf-source-stalled
        // candidate causes.
        let mut moving_descendants: Vec<String> = Vec::new();
        for f in frames {
            if let Some(ls) = f.layout_snapshot.as_ref() {
                for e in &ls.elements {
                    if !e.ancestor_match_selectors.iter().any(|a| a == sel) {
                        continue;
                    }
                    let moved = assessment
                        .moved_selectors
                        .iter()
                        .any(|m| m == &e.selector || e.matched_selectors.iter().any(|x| x == m));
                    if moved && !moving_descendants.contains(&e.selector) {
                        moving_descendants.push(e.selector.clone());
                    }
                }
            }
        }
        if !moving_descendants.is_empty() {
            let sample: Vec<String> = moving_descendants.iter().take(5).cloned().collect();
            hints.push(DiagnosisHint {
                code: "intent-target-parent-of-moving-element".into(),
                target_selector: Some(sel.clone()),
                message: format!(
                    "`{sel}` is declared in `expected_targets` but its own \
                     bbox / opacity / transform stayed static across the sequence \
                     — however {n} of its descendants moved (e.g. {sample:?}). \
                     The agent likely declared the parent while the animation \
                     runs on the children. Re-declare with a descendant \
                     selector (try `{sel} *` or a more specific child class), \
                     or list the moving children explicitly in `expected_targets`. \
                     Reading this hint first short-circuits the per-selector \
                     `raf-source-stalled` chase — the page IS animating, it's \
                     the contract that points at the wrong node.",
                    sel = sel,
                    n = moving_descendants.len(),
                    sample = sample,
                ),
                observed: json!({
                    "moving_descendant_selectors": moving_descendants,
                    "declared_target_static": true,
                }),
                suggested_probe: Some(format!(
                    "frame.layout_probe {{ selectors: [\"{sel}\", \"{sel} *\"] }} \
                     — inspect the parent's bbox + each descendant's bbox / \
                     transform across the sequence and pick the child that \
                     actually animates.",
                    sel = sel,
                )),
            });
            // The descendant pattern is a more specific diagnosis than
            // selector-not-found / hidden-by-zero-opacity / per-selector
            // raf-source-stalled — those would mislead about a target
            // that is, in fact, present and visible. Skip the rest of
            // the per-selector dive.
            continue;
        }

        // selector-not-found: the element never showed up in either snapshot.
        if first_probe.is_none() && last_probe.is_none() {
            hints.push(DiagnosisHint {
                code: "selector-not-found".into(),
                target_selector: Some(sel.clone()),
                message: format!(
                    "Selector `{sel}` was declared in `expected_targets` but \
                     never appeared in any captured layout snapshot. Either \
                     the selector is wrong, the element renders only after a \
                     later trigger, or `include_layout` was off."
                ),
                observed: json!({ "present_in_first_frame": false, "present_in_last_frame": false }),
                suggested_probe: Some(format!(
                    "frame.dom_query {{ js: 'JSON.stringify(document.querySelector(\"{sel}\")?.outerHTML?.slice(0, 200) ?? null)' }}"
                )),
            });
            continue;
        }

        // hidden-by-display-none: zero bbox or display: none / visibility hidden
        if let Some(p) = last_probe {
            let zero_area = p.bbox[2] <= 0.5 || p.bbox[3] <= 0.5;
            if p.display == "none" || zero_area {
                hints.push(DiagnosisHint {
                    code: "hidden-by-display-none".into(),
                    target_selector: Some(sel.clone()),
                    message: format!(
                        "Selector `{sel}` resolved but its computed display is \
                         `{}` and bbox is {:?}. Nothing can animate on an \
                         element that isn't laid out — check the parent's \
                         display / a CSS rule with higher specificity.",
                        p.display, p.bbox
                    ),
                    observed: json!({
                        "display": p.display,
                        "bbox": p.bbox,
                    }),
                    suggested_probe: Some(format!(
                        "frame.dom_query {{ js: '(() => {{ let el = document.querySelector(\"{sel}\"); if (!el) return \"null\"; let s = getComputedStyle(el); return JSON.stringify({{ display: s.display, visibility: s.visibility, opacity: s.opacity }}); }})()' }}"
                    )),
                });
                continue;
            }
        }

        // hidden-by-zero-opacity: opacity stayed 0 across the whole sequence
        if let (Some(a), Some(b)) = (first_probe, last_probe) {
            if a.opacity <= 0.01 && b.opacity <= 0.01 {
                hints.push(DiagnosisHint {
                    code: "hidden-by-zero-opacity".into(),
                    target_selector: Some(sel.clone()),
                    message: format!(
                        "Selector `{sel}` has opacity ~0 in both the first \
                         and last sampled frames. The element exists and is \
                         laid out, but no fade-in occurred. Check whether \
                         the `opacity` keyframe / class toggle actually fires \
                         after the trigger."
                    ),
                    observed: json!({
                        "opacity_first": a.opacity,
                        "opacity_last": b.opacity,
                    }),
                    suggested_probe: Some(format!(
                        "frame.dom_query {{ js: '(() => {{ let el = document.querySelector(\"{sel}\"); return JSON.stringify(el?.getAnimations().map(a => ({{ id: a.id, playState: a.playState, currentTime: a.currentTime }})) ?? []); }})()' }}"
                    )),
                });
                continue;
            }
        }

        // bbox-static-style-mutating: position never moved but some style did
        // (intent probably expected translate / scale but got fade-only)
        if let (Some(a), Some(b)) = (first_probe, last_probe) {
            let bbox_unchanged = (a.bbox[0] - b.bbox[0]).abs() < 0.5
                && (a.bbox[1] - b.bbox[1]).abs() < 0.5
                && (a.bbox[2] - b.bbox[2]).abs() < 0.5
                && (a.bbox[3] - b.bbox[3]).abs() < 0.5;
            let style_changed = a.opacity != b.opacity
                || a.transform != b.transform
                || a.color != b.color
                || a.background_color != b.background_color;
            let expected_motion = intent.is_some_and(|i| {
                i.expected_kinds.iter().any(|k| {
                    matches!(
                        k,
                        MotionCategory::Translate
                            | MotionCategory::Scale
                            | MotionCategory::Layout
                            | MotionCategory::Rotate
                    )
                })
            });
            if bbox_unchanged && style_changed && expected_motion {
                hints.push(DiagnosisHint {
                    code: "bbox-static-style-mutating".into(),
                    target_selector: Some(sel.clone()),
                    message: format!(
                        "Selector `{sel}`'s bbox is unchanged but its style \
                         (opacity / transform / color) did mutate. Intent \
                         expected translate / scale / layout / rotate, but \
                         only colour / opacity changed. The transform may be \
                         applied to a parent or cancelled out."
                    ),
                    observed: json!({
                        "bbox_first": a.bbox,
                        "bbox_last": b.bbox,
                        "transform_first": a.transform,
                        "transform_last": b.transform,
                    }),
                    suggested_probe: Some(format!(
                        "frame.dom_query {{ js: '(() => {{ let el = document.querySelector(\"{sel}\"); if (!el) return \"null\"; let p = el.parentElement; return JSON.stringify({{ self_transform: getComputedStyle(el).transform, parent_transform: p && getComputedStyle(p).transform }}); }})()' }}"
                    )),
                });
            }
        }
    }

    // --- whole-sequence: static page + had triggers ---
    if assessment.is_static && had_triggers {
        hints.push(DiagnosisHint {
            code: "no-dom-mutation-after-trigger".into(),
            target_selector: None,
            message: "Triggers were declared but the captured sequence is \
                      static (no per-frame pixel change). Either the trigger \
                      target selector is wrong (the click landed nowhere), \
                      the trigger fires but the handler is bound elsewhere, \
                      or the animation is on a transform that the layout \
                      probe can't see (canvas / SVG / WebGL)."
                .into(),
            observed: json!({
                "is_static": true,
                "trigger_count": "non-zero",
            }),
            suggested_probe: Some(
                "evidence.list — inspect the TriggerEvent ledger for wait_outcome, \
                 then frame.dom_query the trigger target to confirm the handler is bound."
                    .into(),
            ),
        });
    }

    // --- whole-sequence: static + no triggers + no timeline sources ---
    let total_sources = timeline_sources.css_animations.items.len()
        + timeline_sources.css_transitions.items.len()
        + timeline_sources.waapi.items.len();
    if assessment.is_static && !had_triggers && total_sources == 0 {
        hints.push(DiagnosisHint {
            code: "no-motion-source-registered".into(),
            target_selector: None,
            message: "Page is static, no triggers were declared, and \
                      `timeline.sources` reports zero CSS animations / \
                      transitions / WAAPI animations. Either the verify is \
                      pointed at a static page, or the motion source loads \
                      lazily after the initial sample window."
                .into(),
            observed: json!({
                "css_animations": 0,
                "css_transitions": 0,
                "waapi": 0,
            }),
            suggested_probe: Some(
                "motion.audit_required { url } — confirm the page has no time-driven motion at all."
                    .into(),
            ),
        });
    }

    // --- motion-source-inactive: sources exist but no frame caught them active ---
    if !motion_sources_without_samples.is_empty() && total_sources > 0 {
        // Pick one representative source for the suggested_probe.
        let pick = motion_sources_without_samples.first().cloned();
        hints.push(DiagnosisHint {
            code: "motion-source-inactive".into(),
            target_selector: pick.clone(),
            message: format!(
                "{} motion source(s) are registered in `timeline.sources` but \
                 none of the sampled frames caught them inside their active \
                 window. Either the sample plan misses the source's active \
                 interval, or the source never started (paused / display:none \
                 ancestor / play-state).",
                motion_sources_without_samples.len()
            ),
            observed: json!({
                "uncovered_sources": motion_sources_without_samples,
            }),
            suggested_probe: pick.as_ref().map(|p| {
                format!(
                    "frame.bisect on the largest unobserved interval, OR \
                     frame.dom_query {{ js: '(() => {{ let el = document.querySelector(\"{p}\"); return JSON.stringify(el?.getAnimations().map(a => a.playState) ?? []); }})()' }}"
                )
            }),
        });
    }

    // --- raf-source-stalled: rAF is registered, the page IS animating
    //     SOMETHING (not is_static, some selectors moved), but `intent`'s
    //     expected targets never moved. A very common root cause under
    //     the paused virtual clock is a `performance.now()`-based rAF
    //     loop — `const start = performance.now(); rAF((t) => { dt =
    //     performance.now() - start })` reads `performance.now()` values
    //     that the virtual clock does NOT advance in lock-step with rAF
    //     ticks, so the loop's elapsed-time computation never reaches
    //     its end condition (a standard JS loader stuck at 51% after
    //     1100ms is the canonical case). The fix is to
    //     use the rAF callback's `t` argument as the time origin:
    //     `requestAnimationFrame(t => { if (start === undefined) start
    //     = t; ... })`. A chained reveal that waits on the stuck loop
    //     (loader → `classList.add('ready')` → hero entrance) stays
    //     dead too — this hint surfaces the whole chain in one place.
    //     Advisory — not every undeclared-target failure is this. ---
    if timeline_sources.raf.detected
        && !intent_targets_without_evidence.is_empty()
        && !assessment.is_static
    {
        let stuck: Vec<String> = intent_targets_without_evidence
            .iter()
            .take(3)
            .cloned()
            .collect();
        let target = stuck.first().cloned();
        let probe = target.as_ref().map(|s| {
            format!(
                "frame.dom_query {{ js: 'document.querySelector(\"{s}\")?.outerHTML?.slice(0,160) ?? \"null\"' }} \
             — and grep your source for `performance.now()` next to a `requestAnimationFrame` \
             callback; switch the time origin to the rAF `t` argument."
            )
        });
        hints.push(DiagnosisHint {
            code: "raf-source-stalled".into(),
            target_selector: target,
            message: format!(
                "rAF is registered ({} active callback(s)) and the page IS animating, \
                 but `intent`'s expected target(s) {:?} never moved. This is a \
                 *candidate* — read it as 'one likely cause among several', NOT a \
                 verdict. POSSIBLE causes, in roughly decreasing likelihood for a rAF \
                 page: (1) a `performance.now()`-anchored rAF loop — `const start = \
                 performance.now()` + reading `performance.now() - start` inside the \
                 callback does not advance in lock-step with the virtual rAF tick, so \
                 the loop never reaches its end condition. Fix: use the rAF callback's \
                 `t` argument as the time origin: `requestAnimationFrame(t => {{ if \
                 (start === undefined) start = t; ... }})`. (2) The target selector(s) \
                 are not in the DOM at the sampled times (verify with `frame.dom_query`); \
                 the URL may be serving unexpected content. (3) A chained reveal \
                 depends on a stalled precondition (loader → `classList.add('ready')` \
                 → hero entrance) and the precondition's source is the one stalled, \
                 not the target itself. RULE OUT (2) and (3) before grepping source \
                 for `performance.now()`.",
                timeline_sources.raf.active_count, stuck
            ),
            observed: json!({
                "raf_detected": true,
                "raf_active_count": timeline_sources.raf.active_count,
                // Reliability of this attribution. The hint condition only
                // proves rAF + intent target stuck + non-static page; it
                // does NOT prove the cause is performance.now()-anchored.
                // Emitting a single `performance_now_based_raf_suspected`
                // flag would read as a verdict and mislead the agent — a
                // wrong URL produces the same symptom. List candidate
                // causes instead.
                "candidate_causes": [
                    "performance.now()-anchored rAF loop",
                    "target selector not in DOM at sample times (wrong URL? login wall?)",
                    "chained reveal blocked by a stalled precondition source",
                ],
                "intent_targets_stuck": stuck,
                "is_static": false,
            }),
            suggested_probe: probe,
        });
    }

    // --- css-transition-jumped-over: a CSS `transition:` source is
    //     registered AND the sequence is judged static (is_static:true,
    //     so the non-static gate fails) AND consecutive samples are
    //     spaced wider than the shortest transition duration. The
    //     renderer settled in one step and every middle sample reads
    //     the end state — symptom-identical to "animation never fired"
    //     but the fix is different (shrink sample_plan OR switch to
    //     `@keyframes`). A dedicated code makes the otherwise
    //     indistinguishable "is_static + correctness fail" case a
    //     one-glance diagnosis (symmetric to `raf-source-stalled`).
    //     Advisory — a genuinely static page also trips is_static. ---
    if !timeline_sources.css_transitions.items.is_empty()
        && assessment.is_static
        && frames.len() >= 2
    {
        let min_dur: Option<f64> = timeline_sources
            .css_transitions
            .items
            .iter()
            .filter(|t| t.duration_ms > 0.0)
            .map(|t| t.duration_ms)
            .fold(None, |acc, d| Some(acc.map_or(d, |a: f64| a.min(d))));
        if let Some(min_dur) = min_dur {
            let max_gap = frames
                .windows(2)
                .map(|w| (w[1].t_ms - w[0].t_ms).abs())
                .fold(0.0_f64, f64::max);
            if max_gap >= min_dur {
                let n_trans = timeline_sources.css_transitions.items.len();
                let suggested = (min_dur / 4.0).max(50.0);
                hints.push(DiagnosisHint {
                    code: "css-transition-jumped-over".into(),
                    target_selector: None,
                    message: format!(
                        "{n_trans} CSS `transition:` source(s) are registered (shortest \
                         duration {min_dur:.0}ms) and the sequence reads as static \
                         (`is_static:true`), but consecutive samples are up to \
                         {max_gap:.0}ms apart — `clock.advance` jumped past the \
                         transition window in one step, so the renderer settled before \
                         any middle sample could see it. Symptom-identical to 'animation \
                         never fired', but the fix is different: (1) shrink the sample \
                         step to BELOW the transition duration (e.g. roughly every \
                         {suggested:.0}ms across the transition window), OR (2) rewrite \
                         the `transition:` as `@keyframes` — the CDP Animation domain \
                         picks `@keyframes` up structurally and interpolates per tick \
                         regardless of how coarsely you sampled. Confirm against \
                         `unobserved_intervals[].recommended_next_t_ms` before assuming \
                         a real defect."
                    ),
                    observed: json!({
                        "css_transition_count": n_trans,
                        "shortest_transition_ms": min_dur,
                        "max_sample_gap_ms": max_gap,
                        "is_static": true,
                    }),
                    suggested_probe: Some(format!(
                        "Re-run `motion.verify` with a denser sample_plan around the \
                         transition window (target_times_ms entries spaced < {min_dur:.0}ms), \
                         OR switch the CSS `transition:` to `@keyframes` for end-to-end \
                         observability."
                    )),
                });
            }
        }
    }

    // --- css-animation-delay-overrun: a CSS @keyframes animation has
    //     a delay whose `[delay, delay+duration]` active window does
    //     not overlap the sampled time window. The virtual clock is
    //     pinned at t=0 from navigation, so a long `animation-delay`
    //     (e.g. 2700ms) reads as "already past" if the sampling never
    //     reaches it, AND a sample plan that ends before `delay` reads
    //     the page as still pre-animation — either way the sequence
    //     comes back `is_static: true` even though the animation runs
    //     fine in a real browser. Sister hint to `raf-source-stalled`
    //     and `css-transition-jumped-over` — the third leg of the
    //     "virtual clock × time-origin" trap. ---
    if !timeline_sources.css_animations.items.is_empty()
        && assessment.is_static
        && frames.len() >= 2
    {
        let t0 = frames.first().map(|f| f.t_ms).unwrap_or(0.0);
        let t_n = frames.last().map(|f| f.t_ms).unwrap_or(0.0);
        let misaligned: Vec<&crate::model::DetectedAnimation> = timeline_sources
            .css_animations
            .items
            .iter()
            .filter(|a| {
                a.duration_ms > 0.0 && {
                    let astart = a.delay_ms.max(0.0);
                    let aend = astart + a.duration_ms;
                    // No overlap: active window strictly before or
                    // strictly after the sampled window.
                    aend < t0 || astart > t_n
                }
            })
            .collect();
        if !misaligned.is_empty() {
            let n = misaligned.len();
            let first_anim = misaligned[0];
            let astart = first_anim.delay_ms.max(0.0);
            let aend = astart + first_anim.duration_ms;
            let sel = first_anim.target_selector.clone();
            hints.push(DiagnosisHint {
                code: "css-animation-delay-overrun".into(),
                target_selector: sel.clone(),
                message: format!(
                    "{n} CSS @keyframes animation(s) have an active window that does NOT \
                     overlap the sampled time window. The first one (e.g. on `{}`) is \
                     active at `[{astart:.0}, {aend:.0}]ms` while the sample plan covers \
                     `[{t0:.0}, {t_n:.0}]ms`. The virtual clock pins at t=0 from \
                     navigation, so a long `animation-delay` is either still pending or \
                     already settled by every sample, and the whole sequence reads as \
                     `is_static: true` even though it animates fine in a real browser. \
                     Either (1) shift `sample_plan.target_times_ms` to overlap the \
                     `[delay, delay+duration]` window of the animation, or (2) shorten \
                     `animation-delay` so it fires within your sampled span. Sister case \
                     to `raf-source-stalled` and `css-transition-jumped-over`.",
                    sel.as_deref().unwrap_or("(unnamed)"),
                ),
                observed: json!({
                    "css_animation_count": n,
                    "first_animation_target": sel,
                    "first_animation_active_window_ms": [astart, aend],
                    "sample_window_ms": [t0, t_n],
                    "is_static": true,
                }),
                suggested_probe: Some(format!(
                    "Re-run motion.verify with `sample_plan.target_times_ms` straddling \
                     [{astart:.0}, {aend:.0}]ms (the active window), OR audit the source \
                     for `animation-delay:` values exceeding your sample span."
                )),
            });
        }
    }

    // --- silent-static-prominent-element: a visually prominent element
    //     that never moved while OTHER elements did and `intent` expects
    //     motion, but it was never declared in `expected_targets` so its
    //     stillness never affects pass/fail. Without this hint a hero
    //     headline that never animated passes green simply because it
    //     wasn't a declared target. Advisory — a deliberately static
    //     hero is legitimate, so this is a hint, not a gate. ---
    if let (Some(first), Some(last)) = (first_layout, last_layout) {
        let intent_wants_motion = intent.is_some_and(|i| !i.expected_kinds.is_empty());
        let declared: std::collections::HashSet<&str> = intent
            .map(|i| i.expected_targets.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default();
        let moved: std::collections::HashSet<&str> = assessment
            .moved_selectors
            .iter()
            .map(|s| s.as_str())
            .collect();
        if intent_wants_motion && !moved.is_empty() && !assessment.is_static {
            let vw = if last.viewport_width > 0.0 {
                last.viewport_width
            } else {
                1280.0
            };
            let vh = if last.viewport_height > 0.0 {
                last.viewport_height
            } else {
                800.0
            };
            let varea = (vw * vh).max(1.0);
            let first_by: std::collections::HashMap<&str, &crate::model::ElementProbe> = first
                .elements
                .iter()
                .map(|e| (e.selector.as_str(), e))
                .collect();
            // (element, viewport_fraction, effectively_invisible)
            let mut candidates: Vec<(&crate::model::ElementProbe, f64, bool)> = Vec::new();
            for e in &last.elements {
                let sel = e.selector.as_str();
                if declared.contains(sel) || moved.contains(sel) {
                    continue;
                }
                if e.display == "none" || e.opacity <= 0.01 {
                    continue;
                }
                // Document-root pseudo-elements always span the viewport
                // and never animate themselves (transforms happen on
                // their children); flagging them as "silently static"
                // is pure noise — it would fire on html/body almost
                // every run and drown out the real cases.
                if matches!(sel, "html" | "body") {
                    continue;
                }
                let area = (e.bbox[2] * e.bbox[3]).max(0.0);
                let frac = area / varea;
                // A near-full-viewport box is almost certainly the page
                // shell / a hero container that hosts moving children;
                // its own bbox being still is normal, not a signal.
                if frac >= 0.9 {
                    continue;
                }
                // Fraction of the element's box actually inside the
                // viewport. A hero stuck at `translateY(342px)` under an
                // `overflow:hidden` mask is offset almost entirely off
                // its visible band — area-prominent OR effectively
                // invisible both qualify. An area-only floor would miss
                // this clipped-stuck case.
                let ix = (e.bbox[0] + e.bbox[2]).min(vw) - e.bbox[0].max(0.0);
                let iy = (e.bbox[1] + e.bbox[3]).min(vh) - e.bbox[1].max(0.0);
                let visible_area = ix.max(0.0) * iy.max(0.0);
                let onscreen = if area > 1.0 { visible_area / area } else { 1.0 };
                let prominent = frac >= 0.06;
                let effectively_invisible = area > 1.0 && onscreen < 0.35;
                if !prominent && !effectively_invisible {
                    continue; // small and on-screen — not a silent-break signal
                }
                let Some(fe) = first_by.get(sel) else {
                    continue;
                };
                let still = (fe.bbox[0] - e.bbox[0]).abs() < 2.0
                    && (fe.bbox[1] - e.bbox[1]).abs() < 2.0
                    && (fe.bbox[2] - e.bbox[2]).abs() < 2.0
                    && (fe.bbox[3] - e.bbox[3]).abs() < 2.0
                    && (fe.opacity - e.opacity).abs() < 0.02
                    && fe.transform == e.transform;
                if still {
                    candidates.push((e, frac, effectively_invisible));
                }
            }
            // Effectively-invisible stuck elements rank first (a hidden
            // broken hero is worse than a large static-by-design block),
            // then by viewport area.
            candidates.sort_by(|a, b| {
                b.2.cmp(&a.2)
                    .then(b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
            });
            for (e, frac, invisible) in candidates.into_iter().take(2) {
                let sel = e.selector.clone();
                let message = if invisible {
                    format!(
                        "Element `{sel}` is offset / clipped almost entirely OUT of the \
                         viewport AND never moved across any sampled frame, while {} other \
                         selector(s) DID move and `intent` expects motion. It is not in \
                         `expected_targets`, so pass/fail never saw it — the exact blind \
                         spot where a main element animates from an off-screen / masked \
                         start that never played (GSAP `yPercent` vs CSS `translateY` unit \
                         mismatch, a reveal class / trigger that never toggled). Treat as a \
                         real defect unless it is deliberately hidden.",
                        moved.len()
                    )
                } else {
                    format!(
                        "Prominent element `{sel}` (~{:.0}% of the viewport) stayed \
                         completely still across every sampled frame, while {} other \
                         selector(s) DID move and `intent` expects motion. It is not in \
                         `expected_targets`, so its stillness never moved pass/fail — \
                         the blind spot where an undeclared element is silently broken. \
                         If it is meant to be static, ignore this; if it should animate \
                         it likely never fired (GSAP/CSS unit mismatch, wrong selector, \
                         or a class / trigger that never toggled).",
                        frac * 100.0,
                        moved.len()
                    )
                };
                hints.push(DiagnosisHint {
                    code: "silent-static-prominent-element".into(),
                    target_selector: Some(sel.clone()),
                    message,
                    observed: json!({
                        "selector": sel,
                        "viewport_fraction": frac,
                        "effectively_invisible": invisible,
                        "bbox": e.bbox,
                        "opacity": e.opacity,
                        "transform": e.transform,
                        "moved_selectors_count": moved.len(),
                    }),
                    suggested_probe: Some(format!(
                        "frame.dom_query {{ js: '(() => {{ let el = document.querySelector(\"{sel}\"); if (!el) return \"null\"; return getComputedStyle(el).transform + \" play=\" + el.getAnimations().map(a=>a.playState).join(\",\"); }})()' }}"
                    )),
                });
            }
        }
    }

    // --- intent duration mismatch ---
    if let Some(im) = assessment.intent_match.as_ref() {
        if let Some(dm) = im.duration_match.as_ref() {
            if dm.verdict != "match" {
                hints.push(DiagnosisHint {
                    code: "intent-duration-mismatch".into(),
                    target_selector: None,
                    message: format!(
                        "Coverage {observed:.0}ms vs intent {expected:.0}ms ({verdict}, ratio {ratio:.2}). \
                         Either the sample plan stops too early/late, or the animation duration \
                         was changed (check CSS `animation-duration` / GSAP `duration` / WAAPI options).",
                        observed = dm.observed_coverage_ms,
                        expected = dm.expected_ms,
                        verdict = dm.verdict,
                        ratio = dm.ratio,
                    ),
                    observed: json!({
                        "expected_ms": dm.expected_ms,
                        "observed_coverage_ms": dm.observed_coverage_ms,
                        "ratio": dm.ratio,
                        "verdict": dm.verdict,
                    }),
                    suggested_probe: Some(
                        "timeline.sources — confirm declared durations match the animation source."
                            .into(),
                    ),
                });
            }
        }
        if !im.forbidden_kinds_seen.is_empty() {
            hints.push(DiagnosisHint {
                code: "forbidden-kind-observed".into(),
                target_selector: None,
                message: format!(
                    "Forbidden motion kind(s) observed: {:?}. Intent declared \
                     these as off-limits — fix the source to remove them.",
                    im.forbidden_kinds_seen
                ),
                observed: json!({ "forbidden_kinds_seen": im.forbidden_kinds_seen }),
                suggested_probe: Some(
                    "motion.assess on the same frames — confirm per_transition_kinds shows the forbidden kind.".into(),
                ),
            });
        }
    }

    // --- jank spikes — one hint per event so the agent can drill each.
    //     The message is written PER kind: the two detectors normalise
    //     delta_ratio / z_score differently, so a kind-agnostic "Nσ"
    //     line makes a deliberate fast exit (positional z=624× median)
    //     read like a catastrophic outlier. State the basis inline so
    //     the report is self-explanatory without the skill doc. ---
    for j in &assessment.jank_events {
        let message = match j.kind {
            JankKind::PixelCvOutlier => format!(
                "pixel-cv-outlier: changed-pixel ratio {:.4} at [{:.0}..{:.0}]ms is \
                 {:.1}σ above the per-transition mean (a standard score; same \
                 basis as per_transition_delta). A full-screen beat \
                 (curtain / wipe / hero reveal) trips this even when it is the \
                 intended motion — confirm against `intent` and the contact \
                 sheet before treating it as a defect, don't fail on `passes` \
                 alone.",
                j.delta_ratio, j.from_t_ms, j.to_t_ms, j.z_score,
            ),
            JankKind::PositionalTeleport => format!(
                "positional-teleport: an element's bbox-centre jumped \
                 {:.4}×viewport-width at [{:.0}..{:.0}]ms — that is {:.1}× this \
                 element's median per-interval move, NOT a σ. A large multiple \
                 is expected for a deliberate fast entrance/exit; it only \
                 indicates a defect if `intent` did not expect a discrete move \
                 in this interval. Cross-check the contact sheet.",
                j.delta_ratio, j.from_t_ms, j.to_t_ms, j.z_score,
            ),
        };
        hints.push(DiagnosisHint {
            code: "jank-spike".into(),
            target_selector: None,
            message,
            observed: json!({
                "from_t_ms": j.from_t_ms,
                "to_t_ms": j.to_t_ms,
                "delta_ratio": j.delta_ratio,
                "z_score": j.z_score,
                "kind": j.kind,
            }),
            suggested_probe: Some(format!(
                "frame.bisect {{ episode_id, interval: {{ t0_ms: {:.1}, t1_ms: {:.1} }} }}",
                j.from_t_ms, j.to_t_ms
            )),
        });
    }

    hints
}

fn motion_category_label(c: &MotionCategory) -> &'static str {
    match c {
        MotionCategory::Fade => "fade",
        MotionCategory::Translate => "translate",
        MotionCategory::Scale => "scale",
        MotionCategory::Rotate => "rotate",
        MotionCategory::ColorChange => "color-change",
        MotionCategory::Layout => "layout",
        MotionCategory::Appearance => "appearance",
        MotionCategory::Disappearance => "disappearance",
        MotionCategory::TextChange => "text-change",
    }
}

/// Render the one-line human-readable verdict that lands in
/// `MotionVerifyReport.verdict_human`. Keep it short — it has to be readable
/// in a PR comment, a chat row, or a console log without wrapping.
fn build_verdict_human(
    verdict: &str,
    passes: bool,
    a: &AnimationAssessment,
    coverage_score: f64,
    evidence_missing: &[String],
) -> String {
    if a.is_static && !passes {
        return format!(
            "{verdict} — sequence is static (no per-frame pixel change). \
             Animation never fired or the clock never advanced."
        );
    }
    if let Some(im) = a.intent_match.as_ref() {
        if !im.expected_targets_missing.is_empty() {
            return format!(
                "{verdict} — expected_targets did not move: {:?}. \
                 smoothness={:.2} ({}) jank={} coverage={:.2}",
                im.expected_targets_missing,
                a.smoothness,
                a.smoothness_verdict,
                a.jank_events.len(),
                coverage_score
            );
        }
        if !im.expected_kinds_missing.is_empty() {
            return format!(
                "{verdict} — expected_kinds missing: {:?}. \
                 smoothness={:.2} ({}) jank={}",
                im.expected_kinds_missing,
                a.smoothness,
                a.smoothness_verdict,
                a.jank_events.len()
            );
        }
        if !im.forbidden_kinds_seen.is_empty() {
            return format!(
                "{verdict} — forbidden_kinds observed: {:?}. \
                 smoothness={:.2} ({})",
                im.forbidden_kinds_seen, a.smoothness, a.smoothness_verdict
            );
        }
    }
    let jank_part = if a.jank_events.is_empty() {
        "no jank".to_string()
    } else {
        format!("{} jank events", a.jank_events.len())
    };
    let evidence_part = if evidence_missing.is_empty() {
        String::new()
    } else {
        format!(", {} evidence gaps", evidence_missing.len())
    };
    let intent_part = match a.intent_match.as_ref() {
        Some(im) if im.passes => ", intent_match ok",
        Some(_) => ", intent_match fail",
        None => ", no intent declared",
    };
    format!(
        "{verdict} — smoothness {:.2} ({}), {}{}{}",
        a.smoothness, a.smoothness_verdict, jank_part, intent_part, evidence_part
    )
}
