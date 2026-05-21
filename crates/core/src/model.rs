//! Public model types used by `mcp` and `cli`.
//!
//! Wire schemas are the contract between the agent and ai-motionlens-core.
//! Every type here has stable serde representation and a JSON Schema.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

// === Identifiers ===

macro_rules! id_newtype {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new() -> Self {
                Self(format!("{}-{}", $prefix, Uuid::new_v4()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

id_newtype!(SessionId, "ses");
id_newtype!(EpisodeId, "ep");
id_newtype!(FrameId, "fr");
id_newtype!(ObservationId, "ob");
id_newtype!(TriggerEventId, "tr");
id_newtype!(ArtifactId, "art");

// === Launch ===

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LaunchOptions {
    pub url: String,
    #[serde(default = "default_viewport_w")]
    pub viewport_width: u32,
    #[serde(default = "default_viewport_h")]
    pub viewport_height: u32,
    #[serde(default = "default_true")]
    pub headless: bool,
    #[serde(default = "default_dsf")]
    pub device_scale_factor: f32,
    /// Optional determinism policy applied at launch.  When `None` every
    /// external source is `Live`.  When set:
    /// - `random.Pinned` reseeds `Math.random` and `crypto.getRandomValues`
    ///   from `external_state_seed` so replays are deterministic.
    /// - `storage.Pinned` clears cookies and origin storage at launch.
    /// - `timezone.Pinned` calls `Emulation.setTimezoneOverride("UTC")`.
    /// - `locale.Pinned` calls `Emulation.setLocaleOverride("en-US")`.
    /// - `user_agent.Pinned` calls `Emulation.setUserAgentOverride` with a
    ///   fixed reproducible UA string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_state: Option<ExternalStatePolicy>,
    /// Seed used for `random.Pinned` PRNG. Ignored when `random` is not
    /// `Pinned`. Defaults to `0`.
    #[serde(default)]
    pub external_state_seed: u64,
}

fn default_viewport_w() -> u32 {
    1280
}
fn default_viewport_h() -> u32 {
    800
}
fn default_true() -> bool {
    true
}
fn default_dsf() -> f32 {
    1.0
}

impl LaunchOptions {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            viewport_width: default_viewport_w(),
            viewport_height: default_viewport_h(),
            headless: default_true(),
            device_scale_factor: default_dsf(),
            external_state: None,
            external_state_seed: 0,
        }
    }
}

// === Driver kind / seek strategy ===

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DriverKind {
    VirtualTime,
    InjectedClock,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SeekBackStrategy {
    None,
    /// The driver supports seeking backward by replaying the episode on a
    /// scratch page (the live page is not rewound).
    ReplayScratch,
}

// === External state policy (determinism ledger) ===

/// How the session handles external state during episode replay. Replay can
/// only be deterministic to the extent that external sources are either
/// virtualized (recorded) or pinned. Anything labelled `live` will potentially
/// differ between original capture and replay.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ExternalSourceMode {
    /// The source is passed through to the real environment; replay is not
    /// guaranteed to match the original capture.
    Live,
    /// The source is recorded on first capture and replayed deterministically.
    Recorded,
    /// The source is pinned to a deterministic synthetic value (e.g. seeded RNG).
    Pinned,
    /// The source is unavailable in this configuration.
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExternalStatePolicy {
    pub network: ExternalSourceMode,
    pub service_worker: ExternalSourceMode,
    pub web_socket: ExternalSourceMode,
    pub storage: ExternalSourceMode,
    pub random: ExternalSourceMode,
    pub crypto: ExternalSourceMode,
    pub timezone: ExternalSourceMode,
    pub locale: ExternalSourceMode,
    pub user_agent: ExternalSourceMode,
    pub third_party_iframes: ExternalSourceMode,
    pub media_devices: ExternalSourceMode,
}

impl Default for ExternalStatePolicy {
    /// Everything is live by default. Determinism guarantees are weak;
    /// the agent should treat this as such.
    fn default() -> Self {
        Self {
            network: ExternalSourceMode::Live,
            service_worker: ExternalSourceMode::Live,
            web_socket: ExternalSourceMode::Live,
            storage: ExternalSourceMode::Live,
            random: ExternalSourceMode::Live,
            crypto: ExternalSourceMode::Live,
            timezone: ExternalSourceMode::Live,
            locale: ExternalSourceMode::Live,
            user_agent: ExternalSourceMode::Live,
            third_party_iframes: ExternalSourceMode::Live,
            media_devices: ExternalSourceMode::Disabled,
        }
    }
}

// === Replay cost model ===

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReplayCostModel {
    /// Fixed wall-clock overhead per replay regardless of virtual span (page
    /// reload, navigation, baseline setup).
    pub replay_fixed_cost_ms: f64,
    /// Approximate wall-clock cost to advance 1 virtual millisecond.
    pub cost_per_virtual_ms: f64,
    /// Wall-clock cost of executing one replayed trigger event.
    pub cost_per_trigger_ms: f64,
    /// Wall-clock cost of a single `frame.capture`.
    pub capture_cost_ms: f64,
    /// Probability heuristic in [0.0, 1.0] that the page will block on a
    /// pending network fetch during advance. Higher means replays are more
    /// likely to stall.
    pub network_wait_risk: f64,
}

// === Session capabilities ===

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionCapabilities {
    pub driver: DriverKind,
    /// `true` only if a backward seek is possible via *some* mechanism. Always
    /// answers `true` because scratch-page replay is supported - but the
    /// LIVE page is forward-only (see `live_clock_forward_only`). Read both
    /// fields together: agents that only know about the live page should plan
    /// in ascending order.
    pub can_seek_back: bool,
    pub seek_back_strategy: SeekBackStrategy,
    /// The live page's virtual clock cannot be rewound. `clock.advance` and
    /// `frame.capture_series` move forward only. Any past timestamp must go
    /// through `frame.bisect` or `episode.replay_to`, both of which spin up a
    /// scratch page.
    pub live_clock_forward_only: bool,
    pub supports_css: bool,
    pub supports_waapi: bool,
    pub supports_raf: bool,
    pub supports_scroll_driven: bool,
    pub external_state: ExternalStatePolicy,
    pub replay_cost: ReplayCostModel,
}

// === Clock ===

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ClockPolicy {
    Pause,
    Advance,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClockStatus {
    pub virtual_now_ms: f64,
    pub policy: ClockPolicy,
    pub current_episode_id: Option<EpisodeId>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AdvanceOutcome {
    Advanced,
    StarvationHit,
    NetworkPending,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AdvanceResult {
    pub virtual_now_ms: f64,
    pub outcome: AdvanceOutcome,
}

// === Triggers (recorded into an episode) ===

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum WaitPolicy {
    /// No explicit wait. The trigger fires and `clock.advance` proceeds.
    #[default]
    None,
    /// Block the virtual clock until network is roughly idle after the trigger.
    UntilNetworkIdle,
    /// Block the virtual clock until the next animation frame after the trigger.
    UntilNextFrame,
}

/// How a `Click` / `Hover` trigger reaches the page.
///
/// - `Js` (default) dispatches via the DOM API (`el.click()`,
///   `dispatchEvent(new MouseEvent(...))`).  Runs synchronously on the
///   renderer main thread, never blocks under the paused virtual clock.
///   The trade-off: CSS `:hover`, `pointerdown` / `pointerup`, and
///   coordinate-driven listeners that only fire on real compositor input
///   are not observed.
///
/// - `Cdp` dispatches via `Input.dispatchMouseEvent`, which routes the
///   event through Chromium's compositor pipeline.  Real `:hover` /
///   `pointer*` events fire, but the screenshot path now needs a paint
///   to complete — `Session::trigger` therefore advances the virtual
///   clock by ~16ms after each CDP-mode trigger to flush the
///   compositor.  That 16ms drift is reflected in subsequent
///   `clock.status` / `frame.capture` calls.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum InputMode {
    #[default]
    Js,
    Cdp,
}

/// Result of honoring a `WaitPolicy`. Lets the agent tell the difference
/// between "the network actually quieted" and "we gave up after the soft
/// timeout fired". `wait_for_next_frame` / `wait_for_network_idle` set this
/// truthfully so a caller never has to guess whether the wait succeeded.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum WaitOutcome {
    /// `WaitPolicy::None` was used — nothing was waited for.
    NotRequested,
    /// The requested signal arrived before the soft timeout.
    Satisfied,
    /// The soft timeout elapsed without the signal. The trigger still
    /// dispatched, but downstream code should NOT assume the wait
    /// condition held.
    TimedOut,
}

/// Identifies a frame (browsing context) within the page. `[]` is the main
/// frame; subsequent entries name the iframe path to descend into.
pub type FramePath = Vec<String>;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TargetSpec {
    /// CSS selector resolved when the trigger first fires.
    pub selector: String,
    /// Frame chain to the document that owns the selector. Empty = main frame.
    #[serde(default)]
    pub frame_path: FramePath,
    /// Element position resolved at original capture, used to re-resolve during
    /// replay when the selector becomes ambiguous.
    #[serde(default)]
    pub resolved_coordinates: Option<[f64; 2]>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TriggerKind {
    Click {
        target: TargetSpec,
    },
    Hover {
        target: TargetSpec,
    },
    Type {
        target: TargetSpec,
        text: String,
    },
    Scroll {
        #[serde(default)]
        target: Option<TargetSpec>,
        /// Frame chain even when `target` is None (i.e. scrolling the document
        /// of a nested iframe without a specific element).
        #[serde(default)]
        frame_path: FramePath,
        x: f64,
        y: f64,
    },
    Evaluate {
        js: String,
        /// Frame chain identifying which document the JS should run in.
        #[serde(default)]
        frame_path: FramePath,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TriggerEvent {
    pub trigger_id: TriggerEventId,
    /// Virtual timestamp at which the trigger fires within the episode.
    pub at_t_ms: f64,
    pub kind: TriggerKind,
    #[serde(default)]
    pub wait_policy: WaitPolicy,
    #[serde(default)]
    pub input_mode: InputMode,
    /// Truthful result of honoring `wait_policy`. `Satisfied` means the
    /// `rAF` / `networkAlmostIdle` arrived; `TimedOut` means we gave up
    /// after the soft timeout — downstream observers should NOT treat the
    /// next capture as if the wait condition really held.
    #[serde(default = "default_wait_outcome")]
    pub wait_outcome: WaitOutcome,
}

fn default_wait_outcome() -> WaitOutcome {
    WaitOutcome::NotRequested
}

// === Episode start state ===

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EpisodeStartState {
    /// URL at t=0.
    pub url: String,
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub device_scale_factor: f32,
    /// Wall-clock timestamp (ms since UNIX epoch) when the episode was started.
    /// Used as a label, not for determinism.
    pub started_at_wall_ms: f64,
    /// Effective external-state policy at the moment the episode was started.
    pub external_state: ExternalStatePolicy,
}

// === Frames ===

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ImageFormat {
    #[default]
    Png,
    Jpeg,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    CssAnimation,
    CssTransition,
    WebAnimation,
    Raf,
    Timer,
    Media,
    ScrollDriven,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ActiveSource {
    /// CDP Animation id or synthetic id for non-CDP sources.
    pub id: String,
    pub kind: SourceKind,
    /// `Animation.cssId` from the CDP Animation domain. For named CSS
    /// `@keyframes` on an element with a stable id this is something
    /// like `"#hero-cta"` and is directly usable; for WAAPI or for
    /// elements without a unique id it is a backend-node-id hash like
    /// `"tt2ToBzbmOvcjA=="`. In that case cross-reference with the
    /// frame's `layout_snapshot.elements[].running_animations` (which
    /// lists the running animations' `animationName`s per element) and
    /// with `ActiveSource.name` below to identify the element.
    pub target_selector: Option<String>,
    pub progress: Option<f64>,
    /// `@keyframes` name for CSS animations (e.g. `"fade-up"`) — the
    /// `Animation.name` from the CDP Animation domain. Empty for WAAPI
    /// scripted animations and for CSS transitions. Cross-references
    /// with `layout_snapshot.elements[].running_animations` so the
    /// agent can identify which element a hashed `target_selector`
    /// actually applies to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Source's `activeDuration` in ms (one iteration × iteration
    /// count). `motion.verify` compares this against the sampled
    /// window's total span to detect page-wide ambient loops: when a
    /// source's `duration_ms` exceeds the sample window AND it is
    /// active across every captured `unobserved_intervals[]` entry,
    /// it is classified as ambient and stripped from the per-interval
    /// `active_sources` arrays (kept once in `ambient_source_names`)
    /// so the MCP response doesn't carry N×M ambient entries that
    /// would otherwise dominate it. Note that the CDP Animation domain's
    /// `iterations` field is unreliable for detecting CSS `infinite`
    /// (it consistently reports `1` even for `animation-iteration-count: infinite`),
    /// which is why we infer ambience from the duration-vs-window
    /// shape instead.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub duration_ms: f64,
    /// Human-readable selector this source's animation actually runs
    /// on, resolved by cross-referencing `name` against
    /// `layout_snapshot.elements[].running_animations`. Present when
    /// `target_selector` is a backend-node-id hash and a single
    /// element on the page is running an animation with the matching
    /// `name`. Lets the agent skip the manual cross-reference dance.
    /// Stays `None` when multiple elements share the same
    /// `@keyframes` (a stagger of `.word`s) or when no element's
    /// `running_animations` matches — in those cases the agent still
    /// has the raw cross-reference machinery to disambiguate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_selector_hint: Option<String>,
}

fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Frame {
    pub frame_id: FrameId,
    pub episode_id: EpisodeId,
    pub t_ms: f64,
    /// Custom MCP URI to the full image as an MCP resource. The agent uses
    /// `resources/read` (or the harness equivalent) to fetch the binary blob.
    /// Form: `motionlens://artifacts/<artifact_id>`.
    pub artifact_uri: String,
    /// On-disk path of the artifact image. Use this with the harness `Read`
    /// tool to view the full-resolution frame without going through the MCP
    /// resource API.
    pub artifact_local_path: String,
    /// Small thumbnail base64-inlined for cheap glanceability. Only present
    /// when `capture(..., thumbnail: true)` is requested. Default is `None` to
    /// keep MCP responses lightweight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_base64: Option<String>,
    pub width: u32,
    pub height: u32,
    pub format: ImageFormat,
    pub active_sources: Vec<ActiveSource>,
    /// Optional layout snapshot (per-element bbox + computed style + anomaly
    /// detection). Present only when `capture` was called with `layout: true`.
    /// Use this to catch UI breakage that vision-language models routinely
    /// miss (text overflow, clipped content, off-screen elements, z-index
    /// conflicts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout_snapshot: Option<LayoutSnapshot>,
}

// === Layout probe ===

/// One element's measured layout + most-impactful computed style at capture time.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ElementProbe {
    pub selector: String,
    pub tag: String,
    /// [x, y, width, height] in viewport coordinates (CSS pixels).
    pub bbox: [f64; 4],
    pub opacity: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z_index: Option<String>,
    pub overflow_x: String,
    pub overflow_y: String,
    pub display: String,
    pub position: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_content_preview: Option<String>,
    /// Of the caller-supplied `match_selectors`, the ones this element
    /// actually satisfies via real `Element.matches(selector)` (CSS
    /// semantics, evaluated in the page). Lets `intent.expected_targets`
    /// be any valid CSS selector instead of having to string-equal the
    /// tool's synthesized `#id`-priority notation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched_selectors: Vec<String>,
    /// `@keyframes` / WAAPI animation names currently running on this
    /// element (from `el.getAnimations()[].animationName`). Cross-
    /// reference with `frame.active_sources[].name` to identify which
    /// element a hashed `target_selector` actually applies to — when
    /// CDP returns a backend-node-id hash for `cssId`, agents can
    /// match on `name` instead. Empty when no animations are running
    /// on the element.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub running_animations: Vec<String>,
    /// Of the caller-supplied `match_selectors`, the ones this
    /// element's **strict ancestor** satisfies (via
    /// `el.parentElement.closest(sel)`). Lets diagnostic hints catch
    /// the "agent declared the parent in `expected_targets` but the
    /// animation runs on a child" pattern: a moving child with a
    /// non-empty `ancestor_match_selectors` containing the declared
    /// parent surfaces as `intent-target-parent-of-moving-element`
    /// rather than the much-noisier `raf-source-stalled`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ancestor_match_selectors: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LayoutAnomalyKind {
    /// Element extends past the viewport's right edge.
    HorizontalViewportOverflow,
    /// Element extends past the viewport's bottom edge AND `body` is not
    /// scrollable (true overflow, not normal page scroll).
    VerticalViewportOverflow,
    /// Element's scroll size exceeds its client size and the element is not
    /// configured to scroll - content is clipped/truncated.
    ContentClipped,
    /// Element is fully outside the viewport (no part of its bbox intersects).
    OffScreen,
    /// Multiple positioned elements share the same exact bbox area, suggesting
    /// stacking ambiguity.
    OverlapStacking,
    /// `text-overflow: ellipsis` is applied and `scrollWidth > clientWidth`,
    /// so text is actively truncated.
    TextEllipsisTruncated,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LayoutAnomaly {
    pub kind: LayoutAnomalyKind,
    pub selector: String,
    pub bbox: [f64; 4],
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LayoutSnapshot {
    pub viewport_width: f64,
    pub viewport_height: f64,
    pub document_scroll_width: f64,
    pub document_scroll_height: f64,
    /// Total visible elements found on the page (before truncation).
    pub element_count: u32,
    /// Number of elements actually included in `elements`. May be smaller than
    /// `element_count` if `max_elements` was hit.
    pub elements_returned: u32,
    /// `true` if elements were dropped due to the `max_elements` cap. The
    /// agent should consider raising `max_elements` or narrowing the
    /// `selectors` filter.
    pub truncated: bool,
    pub elements: Vec<ElementProbe>,
    pub anomalies: Vec<LayoutAnomaly>,
    /// `window.scrollX` / `window.scrollY` at the moment the snapshot
    /// was taken. Used downstream (compute_easing_stagger_overshoot)
    /// to back out the viewport-relative bbox shift a scroll trigger
    /// induces — without it, a single `trigger.scroll` jumping 900px
    /// makes every visible element's bbox-center spike by 900px and
    /// every per-target series shows a false `overshoot_event` with
    /// `peak_progress` >> 1 (every reveal card would otherwise spike to
    /// `peak_progress` ~2 immediately after a scroll trigger).
    #[serde(default)]
    pub scroll_x: f64,
    #[serde(default)]
    pub scroll_y: f64,
    /// Selectors of elements that the visibility filter dropped from
    /// `elements` (zero bbox / `display:none` / `visibility:hidden` /
    /// `opacity:0`). They are present in the DOM but suppressed from the
    /// observable element set.
    ///
    /// The transition classifier consults this list so that an element
    /// fading in from `opacity:0` is classified as `fade` rather than
    /// `appearance`, regardless of whether the caller named it in
    /// `expected_targets` (which would otherwise bypass the filter and
    /// keep the element in `elements`). Without this rescue the same
    /// animation reads as `appearance` under `motion.suggest_intent`
    /// (no `expected_targets`) and `fade` under `motion.verify` (with
    /// `expected_targets`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_selectors: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct LayoutProbeOptions {
    /// If set, only elements matching one of these CSS selectors are included
    /// in `elements` (anomaly detection is still run over the whole document).
    #[serde(default)]
    pub selectors: Option<Vec<String>>,
    /// Selectors whose elements should be skipped during anomaly detection.
    /// Use this to silence intentional `overflow: hidden` regions
    /// (SplitText / clipping containers).
    #[serde(default)]
    pub ignore_selectors: Option<Vec<String>>,
    /// CSS selectors to evaluate against every element via real
    /// `Element.matches()`. Each element reports which of these it
    /// satisfies in `ElementProbe.matched_selectors`. Used by
    /// `motion.verify` to grade `intent.expected_targets` semantically
    /// instead of by synthesized-notation string equality. Elements
    /// matching one of these are always retained even when `selectors`
    /// (focus filter) would otherwise drop them.
    #[serde(default)]
    pub match_selectors: Option<Vec<String>>,
    /// Anomaly kinds to suppress entirely (any selector). Use to ignore an
    /// entire class of false positives.
    #[serde(default)]
    pub ignore_anomalies: Option<Vec<LayoutAnomalyKind>>,
    /// Soft cap on the `elements` array. Defaults to 300 when unset.
    /// Anomalies are not capped.
    #[serde(default)]
    pub max_elements: Option<u32>,
}

// === Intervals / sampling ===

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
pub struct Interval {
    pub t0_ms: f64,
    pub t1_ms: f64,
}

impl Interval {
    pub fn new(t0_ms: f64, t1_ms: f64) -> Self {
        Self { t0_ms, t1_ms }
    }

    pub fn span_ms(&self) -> f64 {
        (self.t1_ms - self.t0_ms).abs()
    }

    pub fn midpoint_ms(&self) -> f64 {
        (self.t0_ms + self.t1_ms) / 2.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UnobservedInterval {
    pub interval: Interval,
    pub span_ms: f64,
    pub active_sources: Vec<ActiveSource>,
    pub pixel_delta_hint: Option<f64>,
    pub recommended_next_t_ms: f64,
    pub replay_cost_hint_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CaptureSeriesResult {
    pub frames: Vec<Frame>,
    /// Absolute virtual timestamps that were actually captured, in ascending order.
    pub sampled_points: Vec<f64>,
    pub unobserved_intervals: Vec<UnobservedInterval>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BisectReplayKind {
    /// No replay was needed - the target was forward of the current clock.
    None,
    /// A scratch page was spun up, the episode was replayed, and the frame
    /// was captured there. The live page was not rewound.
    Scratch,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BisectResult {
    pub frame: Frame,
    pub remaining_intervals: Vec<UnobservedInterval>,
    pub replay: BisectReplayKind,
    pub replay_cost_ms: f64,
}

// === Replay result ===

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReplayResult {
    pub virtual_now_ms: f64,
    pub replayed_triggers: u32,
    pub outcome: AdvanceOutcome,
    /// `frame_id` of the frame captured at `virtual_now_ms` on the scratch
    /// page. The frame is persisted into the originating episode's ledger so
    /// it can be passed to `motion.diff` / `motion.assess` later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<FrameId>,
}

// === Timeline detection ===

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DetectionMethod {
    /// Reported authoritatively by the CDP Animation domain (CSS, CSS transition,
    /// WAAPI). Complete for what the domain exposes.
    CdpAnimation,
    /// Discovered via a pre-launch instrumentation hook that monkey-patched
    /// `requestAnimationFrame` / `setTimeout` / `setInterval` before any page
    /// script ran.
    PreloadInstrumentation,
    /// Heuristic post-hoc probe (e.g. counting rAF callbacks since launch).
    /// May miss sources registered before instrumentation attached.
    Heuristic,
    /// Not detected by any available mechanism.
    NotSupported,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DetectionMeta {
    pub method: DetectionMethod,
    /// Self-assessed confidence in [0.0, 1.0].
    pub confidence: f64,
    /// Human-readable description of what this detection can and cannot see.
    pub limitations: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DetectedAnimation {
    pub id: String,
    pub name: Option<String>,
    pub target_selector: Option<String>,
    pub duration_ms: f64,
    pub delay_ms: f64,
    pub iterations: f64,
    pub easing: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScrollDrivenSource {
    pub target_selector: Option<String>,
    pub axis: String,
    pub range_description: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct TimerCounts {
    /// Cumulative `setTimeout` registrations since preload instrumentation
    /// attached.
    pub set_timeout_count: u32,
    /// Cumulative `setInterval` registrations since preload instrumentation
    /// attached.
    pub set_interval_count: u32,
    /// Currently registered timeouts that have not yet fired and have not
    /// been cancelled.
    #[serde(default)]
    pub set_timeout_active: u32,
    /// Currently active intervals.
    #[serde(default)]
    pub set_interval_active: u32,
    /// Cumulative `clearTimeout` calls that successfully cancelled a pending
    /// timeout.
    #[serde(default)]
    pub set_timeout_cancel_count: u32,
    /// Cumulative `clearInterval` calls that successfully cancelled an active
    /// interval.
    #[serde(default)]
    pub set_interval_cancel_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TimelineCategory<T> {
    pub items: Vec<T>,
    pub detection: DetectionMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RafDetectionResult {
    pub detected: bool,
    /// Cumulative `requestAnimationFrame` registrations since preload
    /// instrumentation attached.
    #[serde(default)]
    pub total_count: u32,
    /// rAF callbacks registered but not yet fired and not yet cancelled.
    #[serde(default)]
    pub active_count: u32,
    /// `cancelAnimationFrame` calls that successfully cancelled a pending
    /// rAF.
    #[serde(default)]
    pub cancel_count: u32,
    pub detection: DetectionMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TimerDetectionResult {
    pub counts: TimerCounts,
    pub detection: DetectionMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TimelineSources {
    pub css_animations: TimelineCategory<DetectedAnimation>,
    pub css_transitions: TimelineCategory<DetectedAnimation>,
    pub waapi: TimelineCategory<DetectedAnimation>,
    pub raf: RafDetectionResult,
    pub scroll_driven: TimelineCategory<ScrollDrivenSource>,
    pub media_elements: TimelineCategory<String>,
    pub timers: TimerDetectionResult,
}

// === Motion diff ===

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BboxChange {
    pub selector: String,
    pub from: [f64; 4],
    pub to: [f64; 4],
    pub dx: f64,
    pub dy: f64,
    pub dw: f64,
    pub dh: f64,
    pub opacity_delta: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StyleChange {
    pub selector: String,
    pub property: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MotionDiff {
    pub frame_id_a: FrameId,
    pub frame_id_b: FrameId,
    pub pixel_delta_ratio: f64,
    pub bbox_changes: Vec<BboxChange>,
    pub style_changes: Vec<StyleChange>,
    pub appeared: Vec<String>,
    pub disappeared: Vec<String>,
}

// === DOM query (read-only observation, no episode side effect) ===

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DomQueryResult {
    /// The deserialized JSON value the JS expression evaluated to, or `null`
    /// if it returned `undefined` / a non-serializable type.
    pub value: serde_json::Value,
    /// Number of milliseconds (wall-clock) the evaluation took.
    pub elapsed_ms: f64,
}

// === Animation quality assessment ===

/// Which detector produced a [`JankEvent`]. The two detectors normalise
/// `delta_ratio` and `z_score` on **different bases**, so always read
/// those two fields through the lens of `kind` — comparing a
/// `positional-teleport` `delta_ratio` against `per_transition_delta`
/// (a changed-pixel ratio) is an apples-to-oranges 2-orders-of-magnitude
/// mismatch, not a bug.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum JankKind {
    /// Whole-frame pixel-delta statistical outlier. `delta_ratio` is the
    /// changed-pixel ratio in `[0,1]` (same basis as
    /// `per_transition_delta`); `z_score` is the standard score
    /// `(delta - mean) / stddev` over the per-transition series
    /// (`>= 2.0` = outlier). A deliberate full-screen beat (curtain /
    /// wipe) can trip this even when intentional — cross-check the
    /// interval against `intent` / the contact sheet before treating it
    /// as a defect.
    PixelCvOutlier,
    /// One element's bbox-centre teleported between two samples.
    /// `delta_ratio` is the jump distance / viewport width in `[0,1]`
    /// (NOT a pixel ratio); `z_score` is the jump as a **multiple of
    /// that element's median per-interval displacement** (`>= 4.0` =
    /// teleport), not a standard score. Catches a small element that
    /// jumps far while the changed-pixel ratio stays tiny (so the same
    /// interval's `per_transition_delta` looks near-zero).
    PositionalTeleport,
}

/// A frame-to-frame transition whose motion is unusually large relative to
/// the rest of the sequence. Surfaced to the agent so it can decide whether the
/// spike is a planned beat (e.g. modal appearing) or an unintended jank.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JankEvent {
    pub from_t_ms: f64,
    pub to_t_ms: f64,
    /// Magnitude in `[0,1]`; its basis depends on `kind` (see [`JankKind`]):
    /// changed-pixel ratio for `pixel-cv-outlier`, jump/viewport-width for
    /// `positional-teleport`.
    pub delta_ratio: f64,
    /// Outlier strength; its basis depends on `kind` (see [`JankKind`]): a
    /// standard score for `pixel-cv-outlier`, a median-displacement
    /// multiple for `positional-teleport`.
    pub z_score: f64,
    /// Which detector fired — determines how to read the two fields above.
    pub kind: JankKind,
}

/// Quality assessment over an ordered sequence of captured frames.
///
/// Built so the agent can decide "is this animation good enough?" with
/// structured numbers, not just by looking at images:
/// - `smoothness` ∈ [0.0, 1.0]: 1.0 = constant per-frame delta (smooth);
///   lower = lots of variance (potentially janky).
/// - `jank_events`: per-transition outliers; check whether each is intended.
/// - `coverage_ms`: how much virtual time the sequence spans. Compare to the
///   expected animation duration to judge whether the sample was complete.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AnimationAssessment {
    pub episode_id: EpisodeId,
    pub analyzed_frame_ids: Vec<FrameId>,
    pub frame_count: u32,
    pub coverage_ms: f64,
    pub per_transition_delta: Vec<f64>,
    pub mean_delta: f64,
    pub stddev_delta: f64,
    pub smoothness: f64,
    /// Smoothness threshold verdict: `"good"` (>=0.8), `"acceptable"` (>=0.6),
    /// `"poor"` (<0.6), or `"no-motion"` when the sequence contains no
    /// per-frame pixel change at all. Use this for quick pass/fail decisions
    /// without having to remember the numeric thresholds.
    pub smoothness_verdict: String,
    /// `true` when `mean_delta` is below the no-motion floor — the sequence
    /// is visually static and `smoothness` should not be interpreted as
    /// "smooth motion". Static pages always have CV = 0, which would
    /// otherwise return `smoothness: 1.0`.
    pub is_static: bool,
    pub jank_events: Vec<JankEvent>,
    /// Aggregate of motion categories detected across the sequence (derived
    /// from per-frame layout snapshots, when present). Empty if frames did
    /// not include layout_snapshot.
    pub detected_motion_kinds: Vec<MotionCategory>,
    /// Selectors whose `bbox` / `opacity` / `transform` changed between any
    /// two adjacent frames in the sequence. Drives `intent_match`'s
    /// `expected_targets_seen` and is the structured complement to
    /// `detected_motion_kinds`. Empty when the frames don't carry layout
    /// snapshots.
    pub moved_selectors: Vec<String>,
    /// Selectors that newly appeared (DOM-new node, was hidden but
    /// returned to non-hidden state via fade/show only when paired
    /// with the `hidden_selectors` rescue elsewhere). Used to scope
    /// `forbidden_kinds: ["appearance"]` to the specific elements
    /// the agent named in `expected_targets`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub appeared_selectors: Vec<String>,
    /// Selectors that disappeared from the DOM mid-sequence (or
    /// drifted out of view in a way the visibility filter dropped).
    /// Paired with `appeared_selectors` to scope
    /// `forbidden_kinds: ["disappearance"]` to the agent's declared
    /// targets, so scrolling that pushes unrelated sections off the
    /// viewport does not false-fail the gate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disappeared_selectors: Vec<String>,
    /// Per-transition detected categories aligned to `per_transition_delta`.
    pub per_transition_kinds: Vec<Vec<MotionCategory>>,
    /// Per-selector observed motion curve and best-matched easing template.
    /// One entry per `moved_selectors` element when the layout snapshot
    /// carries enough samples (≥ 3 frames) to fit an easing curve.
    #[serde(default)]
    pub per_target_easing: Vec<TargetEasing>,
    /// `[0.0..1.0]` uniformity of motion onset times across multiple
    /// targets. `1.0` = perfectly uniform stagger, `0.0` = no detectable
    /// pattern. `None` when fewer than two targets moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stagger_uniformity: Option<f64>,
    /// Targets that overshot their final value before settling. Useful to
    /// distinguish intentional spring / bounce easing from unintended
    /// overshoot bugs.
    #[serde(default)]
    pub overshoot_events: Vec<OvershootEvent>,
    /// Long-Animation-Frame entries the renderer reported while the
    /// sequence was being captured. Each entry indicates the main
    /// thread blocked for `duration` ms on a single frame, with
    /// `blocking_duration` of that being scheduler-blocked time.
    /// Empty under a fully paused virtual clock — entries are
    /// normally produced during `clock.advance` budget burn or the
    /// paint flush after a CDP-mode trigger.
    #[serde(default)]
    pub runtime_jank_events: Vec<RuntimeJankEvent>,
    /// Comparison against the episode's recorded `intent`, if any.
    pub intent_match: Option<IntentMatchReport>,
    pub summary: String,
    pub recommendations: Vec<String>,
}

/// Observed motion curve for one selector along its most-moving axis,
/// plus the best-fit `EasingHint` from a fixed catalog (linear / ease /
/// ease-in / ease-out / ease-in-out / ease-out-cubic / ease-in-cubic).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TargetEasing {
    pub selector: String,
    /// Which signal we tracked. `"translate-x" | "translate-y" |
    /// "opacity" | "scale-x" | "scale-y"`. The axis with the largest
    /// peak-to-trough magnitude is chosen automatically.
    pub axis: String,
    /// Sampled values for `axis` at each captured frame (raw, not
    /// normalised). Useful to reproduce the curve.
    pub observed_values: Vec<f64>,
    /// Same length as `observed_values`, normalised to `[0..1]` so the
    /// curve can be compared against an easing template directly.
    pub observed_progress: Vec<f64>,
    /// Best-matching easing template by RMS residual.
    pub best_match_easing: String,
    /// RMS residual between `observed_progress` and the best-match
    /// template's progress at the same normalised times. Lower = better
    /// fit. Above ~0.10 usually indicates the motion doesn't follow a
    /// standard easing curve.
    pub rms_error: f64,
    /// `t_ms` at which this target first crossed 5% normalised
    /// progress. Used by `intent_match.duration_match` to measure the
    /// observed motion span against the expected_targets, instead of
    /// the whole sample_plan span (which a long-tail / ambient
    /// background animation would inflate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onset_ms: Option<f64>,
    /// `t_ms` at which this target reached >=95% of its final
    /// normalised progress and stayed there for the rest of the
    /// sequence. `None` when the motion never crossed 95% within the
    /// sampled window (still in flight or never settles).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle_ms: Option<f64>,
}

/// One overshoot event: a target's observed progress went above 1.0
/// (or below 0.0) before settling at the final value.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OvershootEvent {
    pub selector: String,
    pub axis: String,
    /// Maximum overshoot ratio (>=1.0 means the target went past its
    /// final value; e.g. 1.12 = 12% overshoot before settling).
    pub peak_progress: f64,
    /// `t_ms` at which the peak overshoot was observed.
    pub peak_t_ms: f64,
}

/// One Long-Animation-Frame entry as the renderer reported it. Maps 1:1
/// to a `PerformanceEntry` of type `long-animation-frame` (Chrome 123+).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeJankEvent {
    /// Wall-clock start of the offending frame, in ms since page load.
    pub start_time_ms: f64,
    /// Total frame duration in ms (entries are emitted when this
    /// exceeds Chrome's internal threshold, currently ~50ms).
    pub duration_ms: f64,
    /// Time spent waiting for the renderer to start rendering this
    /// frame after the main thread became idle.
    pub render_start_ms: f64,
    /// Time spent on style / layout for this frame.
    pub style_and_layout_start_ms: f64,
    /// Of `duration_ms`, the portion the renderer was actively blocked
    /// by long tasks (script execution, layout, etc.).
    pub blocking_duration_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IntentMatchReport {
    /// Echo of `intent.description` for traceability in `evidence.list`.
    pub description: String,
    pub expected_kinds_seen: Vec<MotionCategory>,
    pub expected_kinds_missing: Vec<MotionCategory>,
    pub forbidden_kinds_seen: Vec<MotionCategory>,
    /// Selectors declared in `intent.expected_targets` that DID move
    /// during the sequence (bbox / opacity / transform change in any
    /// adjacent frame pair).
    #[serde(default)]
    pub expected_targets_seen: Vec<String>,
    /// Selectors declared in `intent.expected_targets` that were NOT
    /// observed to move during the sequence. A non-empty list flips
    /// `passes` to `false`.
    #[serde(default)]
    pub expected_targets_missing: Vec<String>,
    /// Ratio of observed `coverage_ms` to `intent.expected_duration_ms`. None
    /// if `expected_duration_ms` was not declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_match: Option<DurationMatch>,
    /// Easing match — present when `intent.expected_easing` is declared.
    /// Compares the declared easing template against the best-matched
    /// template inferred from `per_target_easing` (majority vote).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub easing_match: Option<EasingMatch>,
    /// Scroll-progress range comparison for scroll-driven animations.
    /// `None` outside `recipes.scroll_animation_check` and when
    /// `expected_progress_range` was not declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_range_match: Option<ScrollRangeMatch>,
    /// `true` if expected kinds are all present AND no forbidden kinds appear
    /// AND `expected_targets_missing` is empty AND `duration_match` (if any)
    /// verdict is `"match"` AND `scroll_range_match` (if any) verdict is
    /// `"match"`.
    pub passes: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DurationMatch {
    pub expected_ms: f64,
    pub observed_coverage_ms: f64,
    pub ratio: f64,
    pub verdict: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EasingMatch {
    /// Echo of `intent.expected_easing` as the canonical kebab-case label.
    pub expected: String,
    /// Most common `best_match_easing` across the per-target fits.
    pub observed: String,
    /// `true` when `expected == observed`. Flips `intent_match.passes` to
    /// `false` when it doesn't.
    pub passes: bool,
}

/// Easing template label the agent can declare in `EpisodeIntent` and
/// that `motion.assess` matches against. The catalog is intentionally
/// small — easing names beyond this set should be declared via `Custom`
/// (no curve fit, advisory only).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum EasingHint {
    Linear,
    Ease,
    EaseIn,
    EaseOut,
    EaseInOut,
    EaseInCubic,
    EaseOutCubic,
    EaseInOutCubic,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScrollRangeMatch {
    pub expected_start: f64,
    pub expected_end: f64,
    pub observed_start: f64,
    pub observed_end: f64,
    /// "match" | "outside-expected" | "narrower-than-expected" | "wider-than-expected"
    pub verdict: String,
}

// === Evidence ledger ===

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Observation {
    pub observation_id: ObservationId,
    pub episode_id: EpisodeId,
    pub interval: Option<Interval>,
    pub claim: String,
    pub confidence: f64,
    pub evidence_frame_ids: Vec<FrameId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct EvidenceLedger {
    pub start_state: Option<EpisodeStartState>,
    /// Agent-declared intent for this episode (animation Plan from SKILL Step 1).
    pub intent: Option<EpisodeIntent>,
    pub triggers: Vec<TriggerEvent>,
    pub frames: Vec<Frame>,
    pub diffs: Vec<MotionDiff>,
    pub observations: Vec<Observation>,
}

// === Episode intent (Plan recorded before observing) ===

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EpisodeIntent {
    /// Free-form description of what the animation should feel like.
    pub description: String,
    /// Expected animation duration in milliseconds (TIME-driven animations).
    /// Used by `motion.assess` to compute `intent_match.duration_match`.
    #[serde(default)]
    pub expected_duration_ms: Option<f64>,
    /// Expected scroll-progress range for SCROLL-driven animations, as
    /// `[start, end]` in `[0..1]`. Used by `recipes.scroll_animation_check`
    /// to compute `intent_match.scroll_range_match`. Use this INSTEAD of
    /// `expected_duration_ms` for scrub / pin / parallax flows.
    #[serde(default)]
    pub expected_progress_range: Option<[f64; 2]>,
    /// Expected motion kinds. Reasons: vision LMs sometimes "see" motion that
    /// isn't there (or miss subtle motion). Declaring expectations lets the
    /// assessment flag mismatches.
    #[serde(default)]
    pub expected_kinds: Vec<MotionCategory>,
    /// Selectors that the agent expects to move.
    #[serde(default)]
    pub expected_targets: Vec<String>,
    /// Forbidden motions the agent wants the assessment to flag if they appear.
    /// E.g. `expected_kinds=[Fade]` plus `forbidden_kinds=[Translate]` means
    /// "fade only, no translate".
    #[serde(default)]
    pub forbidden_kinds: Vec<MotionCategory>,
    /// Expected easing template. When set, `motion.assess` fits each
    /// `per_target_easing` to the standard easing catalog and majority-votes
    /// to produce `intent_match.easing_match`. Leave `None` for animations
    /// where the easing is intentionally varied across targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_easing: Option<EasingHint>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum MotionCategory {
    Fade,
    Translate,
    Scale,
    Rotate,
    ColorChange,
    Layout,
    Appearance,
    Disappearance,
    /// An element's text content changed between frames — e.g. a Stats
    /// count-up rolling `0 → 247`, a typewriter writing characters,
    /// a label flipping between localised strings. The bbox / opacity
    /// / transform may not move, so the other classifier branches miss
    /// it.
    TextChange,
}

// === Video export ===

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum VideoFormat {
    /// Animated GIF. Universally readable; lossy 256-color palette.
    #[default]
    Gif,
    /// Animated PNG. Lossless, larger files. Read by modern browsers and
    /// most image viewers.
    Apng,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VideoExport {
    pub artifact_uri: String,
    pub artifact_local_path: String,
    pub format: VideoFormat,
    pub width: u32,
    pub height: u32,
    pub frame_count: u32,
    pub fps: f64,
    pub duration_ms: f64,
}

// === Contact sheet (time-strip) ===

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContactSheetCell {
    pub frame_id: FrameId,
    pub column: u32,
    pub t_ms: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_y: Option<f64>,
    /// Human-readable label that is also burned into the contact-sheet image
    /// under the corresponding column.
    pub label: String,
}

/// One-shot full-page PNG of the page's final settled state. Used by
/// `MotionVerifyReport.final_fullpage_screenshot` to obviate the
/// `browser_take_screenshot` anti-pattern.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FullpageScreenshot {
    /// Custom MCP URI to the full image as an MCP resource
    /// (`motionlens://artifacts/<artifact_id>`).
    pub artifact_uri: String,
    /// On-disk path for the harness `Read` tool.
    pub artifact_local_path: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContactSheet {
    pub artifact_uri: String,
    pub artifact_local_path: String,
    pub width: u32,
    pub height: u32,
    pub columns: u32,
    pub rows: u32,
    pub frame_ids: Vec<FrameId>,
    /// Per-column structured metadata. The `label` text on each cell is also
    /// rendered onto the image strip.
    pub cells: Vec<ContactSheetCell>,
}

// === Scroll series ===

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScrollSeriesResult {
    pub frames: Vec<Frame>,
    /// Actual scroll Y position for each captured frame, in CSS pixels.
    pub scroll_positions: Vec<f64>,
    /// Scroll progress values [0..1] that were requested (document-level).
    pub requested_progress: Vec<f64>,
    pub document_scroll_height: f64,
    pub viewport_height: f64,
}

/// Specifies scroll-progress values RELATIVE TO A SECTION rather than the
/// whole document. `values` in `[0..1]` are mapped to document-level progress
/// based on the section's intersection with the viewport.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SectionProgressSpec {
    /// CSS selector for the section element.
    pub selector: String,
    /// Progress values in [0..1] inside the section. `0` = section just enters
    /// the viewport from below, `1` = section just leaves the viewport above.
    pub values: Vec<f64>,
}

// === High-level recipe: scroll animation check ===

/// Which parts of `ScrollAnimationCheckResult` to compute and return.
/// Default is `[Series, ContactSheet, Assessment]` (everything).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RecipeInclude {
    Series,
    ContactSheet,
    Assessment,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScrollAnimationCheckResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<ScrollSeriesResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_sheet: Option<ContactSheet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessment: Option<AnimationAssessment>,
    /// Gates the recipe surfaces alongside `assessment`. Currently
    /// populated with the `scroll-range-match` advisory entry when
    /// `assessment.intent_match.scroll_range_match.verdict != "match"`
    /// — that case is a contract-vs-observation alignment issue
    /// (often `position: sticky` / pinned section whose ScrollTrigger
    /// active range differs from the section's bbox progress), so it
    /// surfaces with `reliability: advisory` and is paired with a
    /// detail message pointing the caller at the likely fix. Empty
    /// when the scroll range matches or no `expected_progress_range`
    /// was declared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_gates: Vec<FailedGate>,
}

// === Motion audit (gate input) ===

/// Result of `motion.audit_required`. Surfaces whether the page contains
/// time-dependent UI (CSS / WAAPI / rAF / scroll-driven / library-driven) and
/// therefore needs an `motion.verify` evidence pass before declaring "done".
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MotionAuditResult {
    /// `true` when at least one motion source is present (CSS animation,
    /// CSS transition, WAAPI, rAF, scroll-driven, media). Drives the
    /// `required_verification` flag.
    pub motion_detected: bool,
    /// Full timeline source inventory used to make the decision. Callers can
    /// inspect this to choose sample density / focus selectors.
    pub sources: TimelineSources,
    /// `true` when the page has motion and therefore `motion.verify`
    /// should run before the caller treats the page as verified.
    pub required_verification: bool,
    /// Sample density recommendation derived from the longest detected
    /// motion duration (or a default sweep when only rAF / scroll-driven are
    /// present). Ascending, in ms. Pass these to `motion.verify`'s
    /// `sample_plan.target_times_ms`.
    pub recommended_target_times_ms: Vec<f64>,
    /// Human-readable reason for the verdict (e.g. "CSS animation 'fade-in'
    /// of duration 600ms detected"). Kept short on purpose for prompt / CI
    /// output.
    pub reason: String,
}

// === Motion verify (gate output) ===

/// Per-page viewport / headless override for `MotionContract`. When omitted
/// `motion.verify` uses the defaults baked into `LaunchOptions`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MotionViewport {
    #[serde(default = "default_viewport_w")]
    pub width: u32,
    #[serde(default = "default_viewport_h")]
    pub height: u32,
    #[serde(default = "default_dsf")]
    pub device_scale_factor: f32,
    #[serde(default = "default_true")]
    pub headless: bool,
}

impl MotionViewport {
    pub fn to_launch_options(
        &self,
        url: String,
        external_state: Option<ExternalStatePolicy>,
        external_state_seed: u64,
    ) -> LaunchOptions {
        LaunchOptions {
            url,
            viewport_width: self.width,
            viewport_height: self.height,
            headless: self.headless,
            device_scale_factor: self.device_scale_factor,
            external_state,
            external_state_seed,
        }
    }
}

impl Default for MotionViewport {
    fn default() -> Self {
        Self {
            width: default_viewport_w(),
            height: default_viewport_h(),
            device_scale_factor: default_dsf(),
            headless: default_true(),
        }
    }
}

/// One trigger to fire at a specific virtual time during a `motion.verify`
/// run. The order is preserved; ties at the same `at_t_ms` fire in array
/// order.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContractTrigger {
    pub at_t_ms: f64,
    pub kind: TriggerKind,
    #[serde(default)]
    pub wait_policy: WaitPolicy,
    /// How the trigger reaches the page. Defaults to `Js` (synchronous
    /// DOM dispatch); set to `Cdp` to fire real compositor input via
    /// `Input.dispatchMouseEvent` — required for `:hover` / `pointer*`
    /// / coordinate-driven listeners.
    #[serde(default)]
    pub input_mode: InputMode,
}

/// Sampling plan for `motion.verify`. The verifier advances the virtual
/// clock to each absolute timestamp in `target_times_ms` (ascending) and
/// captures one frame at each point.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SamplePlan {
    /// Ascending absolute virtual times in ms.
    pub target_times_ms: Vec<f64>,
    /// When `true`, every captured frame carries a full
    /// `layout_snapshot` (no selector filter). When `false`, frames are
    /// image-only — lighter responses, but `expected_targets` cannot be
    /// evaluated.
    #[serde(default = "default_true")]
    pub include_layout: bool,
    /// Optional selector filter applied to layout snapshots, when
    /// `include_layout` is `true`. Forwarded to
    /// `frame.layout_probe.options.selectors`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_selectors: Option<Vec<String>>,
}

/// Pass/fail thresholds for `motion.verify`. The verifier flips `passes` to
/// `false` whenever any threshold is violated.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MotionThresholds {
    /// `motion.assess.smoothness` must be `>= min_smoothness` (when the
    /// sequence is not static).
    #[serde(default = "default_min_smoothness")]
    pub min_smoothness: f64,
    /// `motion.assess.jank_events.len()` must be `<= max_jank_events`.
    #[serde(default)]
    pub max_jank_events: u32,
    /// When `true`, `intent_match.passes` must be `true`. Implies the
    /// caller declared `episode_intent`.
    #[serde(default = "default_true")]
    pub require_intent_match: bool,
    /// `coverage_score` (samples vs. recommended density) must be
    /// `>= min_coverage_score`.
    #[serde(default = "default_min_coverage_score")]
    pub min_coverage_score: f64,
    /// When `true`, an `is_static` sequence (no per-frame pixel change)
    /// is treated as a failure — useful to catch animations that never
    /// fired at all.
    #[serde(default = "default_true")]
    pub require_non_static: bool,
}

impl Default for MotionThresholds {
    fn default() -> Self {
        Self {
            min_smoothness: default_min_smoothness(),
            max_jank_events: 0,
            require_intent_match: true,
            min_coverage_score: default_min_coverage_score(),
            require_non_static: true,
        }
    }
}

fn default_min_smoothness() -> f64 {
    0.6
}
fn default_min_coverage_score() -> f64 {
    0.6
}

/// Full input to `motion.verify`. Can be authored as `motionlens.config.json`
/// in a repo and shared between the MCP tool, the `ai-motionlens verify`
/// CLI, the `animation-qa` sub-agent, and CI.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MotionContract {
    pub url: String,
    #[serde(default)]
    pub viewport: MotionViewport,
    /// Plan declaration: description / expected duration / expected motion
    /// kinds / expected target selectors / forbidden kinds. Required when
    /// `thresholds.require_intent_match` is `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_intent: Option<EpisodeIntent>,
    /// Triggers to fire in order before / during sampling. Omit for a
    /// static-load animation that auto-runs on page load.
    #[serde(default)]
    pub triggers: Vec<ContractTrigger>,
    pub sample_plan: SamplePlan,
    #[serde(default)]
    pub thresholds: MotionThresholds,
    /// When `true`, also produce a `motion.contact_sheet` of the captured
    /// frames so the report carries a single shareable time-strip image.
    #[serde(default = "default_true")]
    pub include_contact_sheet: bool,
    /// When `true`, the returned `frames[]` carry their full
    /// `layout_snapshot` (DOM bbox + computed style) and `thumbnail_base64`.
    /// Default `false` to keep responses small: `assessment`,
    /// `contact_sheet` (with its DOM-truth overlay), `diagnosis_hints` and
    /// `coverage_score` are computed server-side from the captured layout
    /// *before* the response is built, so stripping it from `frames[]`
    /// leaves the pass/fail verdict and every score unchanged. Set `true`
    /// only when the agent needs the raw per-frame DOM snapshot inline;
    /// otherwise drill a specific element on demand with
    /// `frame.layout_probe` / `frame.dom_query`.
    #[serde(default)]
    pub include_frame_detail: bool,
    /// When set, render the captured frames as an animated GIF / APNG via
    /// `motion.video_export` and include it in the report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_video: Option<VideoFormat>,
    /// When `true`, capture a single **full-page** PNG of the page's
    /// final settled state (after the sample plan completes) and
    /// attach it to the report. `contact_sheet` is a time-strip
    /// that's perfect for triaging the animation itself, but it
    /// isn't the right artefact for "does the whole rendered page
    /// look right at the end?" — without this option an agent would
    /// reach for Playwright `browser_take_screenshot` to get a
    /// fullpage shot, which the skill explicitly prohibits. Default
    /// `false` to keep responses small; flip to `true` when reviewing
    /// the page visually.
    #[serde(default)]
    pub include_final_fullpage_screenshot: bool,
    /// Optional determinism policy forwarded to `Session::launch`. When
    /// `None`, every external source is `Live` (the gate runs against the
    /// real environment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_state: Option<ExternalStatePolicy>,
    /// Seed for `random.Pinned`. Ignored unless the policy pins random.
    #[serde(default)]
    pub external_state_seed: u64,
}

/// How much to trust a failed gate. The headline `passes` bool ANDs five
/// gates, but they are NOT equally reliable: `intent-match` / `non-static`
/// assert real correctness, whereas `smoothness` / `jank-events` /
/// `coverage` are advisory metrics that produce false negatives on
/// legitimate work (a deliberate full-screen beat reads as a pixel-cv
/// jank; a `scaleX` bar reads as a positional teleport; rAF/GSAP yields
/// `coverage 0`; a count→burst loader is "rough" by construction). So a
/// `passes:false` driven only by `advisory` gates is likely a
/// measurement / choreography artifact, not a quality defect — this is
/// surfaced in `failed_gates` so the agent reads the headline correctly.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GateReliability {
    /// The gate asserts genuine correctness — trust a failure here as a
    /// real defect (intent not satisfied, or the animation never fired).
    Correctness,
    /// The gate is an advisory metric prone to false negatives on
    /// legitimate animation — triage with `intent_match` + the contact
    /// sheet before treating a failure as a defect.
    Advisory,
}

/// One pass/fail gate that `passes:false` is attributable to, with how
/// much that failure should be trusted. Lets the agent / CI see *why*
/// the headline bool flipped and whether it is a real defect or a
/// known-noisy advisory metric — so the most prominent field
/// (`passes`) is never read without its reliability context.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FailedGate {
    /// `"smoothness"` | `"jank-events"` | `"intent-match"` |
    /// `"coverage"` | `"non-static"`.
    pub gate: String,
    pub reliability: GateReliability,
    /// Human-readable specifics (observed vs threshold).
    pub detail: String,
}

/// Full output of `motion.verify`. Also persisted to disk as
/// `motionlens-report.json` so CI / sub-agents / humans can consume the
/// same artifact.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MotionVerifyReport {
    /// URL the contract requested. Persisted so downstream consumers
    /// (`ai-motionlens gate-check`, CI) can verify the report was produced
    /// for the same URL they intend to gate.
    pub contract_url: String,
    /// Viewport the contract requested. Same purpose as `contract_url`.
    pub contract_viewport: MotionViewport,
    /// `document.title` of whatever the URL actually served, probed once
    /// right after navigation. Lets a reader catch a port-collision /
    /// redirect / wrong-build situation at a glance instead of mistaking
    /// it for an animation defect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_title: Option<String>,
    /// `location.href` of whatever the URL actually served (after any
    /// redirect). Compare against `contract_url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_url: Option<String>,
    /// `true` when every threshold passes AND every expected target moved
    /// AND no forbidden motion appeared AND the sequence is non-static
    /// (unless `thresholds.require_non_static` is `false`).
    pub passes: bool,
    /// Three-value verdict, derived from `passes` + `confidence`. Easier for
    /// agents and CI to pattern-match on than the boolean alone:
    ///
    /// - `"pass"`            — `passes: true` AND `confidence >= 0.7`
    /// - `"needs-attention"` — `passes: true` AND `confidence < 0.7`
    ///   (the contract passed, but the gate isn't sure — usually
    ///   `coverage_score` is low or `evidence_missing` is non-empty)
    /// - `"fail"`            — `passes: false`
    pub verdict: String,
    /// One-line human-readable summary of the verdict. The agent or CI can
    /// surface this directly in a PR comment, a chat message, or a console
    /// log without having to interpret the raw numbers. Example:
    /// `"pass — smooth (0.82) modal fade, intent_match ok, no jank"` or
    /// `"fail — hero never moved (intent_targets_without_evidence: #hero)"`.
    pub verdict_human: String,
    /// Which pass/fail gate(s) `passes:false` is attributable to, each with
    /// a `reliability`. Empty when `passes` is `true`. **Read this before
    /// trusting `passes:false`**: if every entry is `advisory` (smoothness
    /// / jank-events / coverage) the failure is likely a measurement /
    /// choreography artifact, not a quality defect; a `correctness` entry
    /// (intent-match / non-static) is a real defect. The single most
    /// reliable signal remains `assessment.intent_match.passes`.
    #[serde(default)]
    pub failed_gates: Vec<FailedGate>,
    /// `[0.0..1.0]` rough confidence the verdict is meaningful. Climbs with
    /// `coverage_score` and the absence of `evidence_missing` entries.
    pub confidence: f64,
    pub session_id: SessionId,
    pub episode_id: EpisodeId,
    pub assessment: AnimationAssessment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_sheet: Option<ContactSheet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoExport>,
    /// Full-page PNG of the page's final settled state, taken after
    /// the sample plan completes. `Some` only when the contract set
    /// `include_final_fullpage_screenshot: true`. This is the artefact
    /// an agent would otherwise reach for `browser_take_screenshot` to
    /// get — surfacing it inside the gate keeps the no-Playwright
    /// invariant holding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_fullpage_screenshot: Option<FullpageScreenshot>,
    /// Structured reasons the gate is incomplete. Empty when fully observed
    /// and `passes: true`. Examples: `"intent_targets_without_evidence: #hero"`,
    /// `"motion_source 'fade-in' has no sample inside its active window"`.
    pub evidence_missing: Vec<String>,
    /// `[0.0..1.0]` ratio of sampled points that landed inside detected
    /// motion windows over the total recommended density.
    pub coverage_score: f64,
    /// CDP-detected motion sources (by id / target) that had no sampled
    /// frame inside their `[start_time, start_time + duration]` window.
    pub motion_sources_without_samples: Vec<String>,
    /// Subset of `intent.expected_targets` that did not register any
    /// movement under the current sampling.
    pub intent_targets_without_evidence: Vec<String>,
    /// Midpoint of the largest unobserved interval — the next-best place
    /// to sample if the caller wants to lift `coverage_score`. `None` when
    /// there are no gaps left worth drilling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_required_observation: Option<f64>,
    pub timeline_sources: TimelineSources,
    pub frames: Vec<Frame>,
    pub unobserved_intervals: Vec<UnobservedInterval>,
    /// Animation names (CSS `@keyframes` / WAAPI ids) that are active
    /// across every single `unobserved_intervals` entry — i.e.
    /// page-wide ambient loops (marquees, `orb-float`-style background
    /// drifts, `mark-spin` brand rotations). They are stripped from
    /// the per-interval `active_sources` arrays in the MCP response
    /// (kept in the on-disk full report so `Read` of
    /// `artifact_local_path` still has everything) so the wire
    /// payload doesn't redundantly carry N×M ambient entries that
    /// would otherwise dominate the response and trigger
    /// `response_truncated_for_mcp_limit`. Empty when no source is
    /// active in every interval, or when `include_frame_detail: true`
    /// keeps the full per-interval expansion.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ambient_source_names: Vec<String>,
    /// On-disk path to the canonical `motionlens-report.json` artifact in
    /// the session's artifact store. Always written before this struct is
    /// returned, so the agent can `Read` it without checking existence.
    pub artifact_local_path: String,
    /// On-disk path to the **cwd copy** of the report. `Some` when
    /// `./motionlens-report.json` was written successfully, `None` when
    /// the cwd was read-only or the write otherwise failed. The canonical
    /// copy is always at `artifact_local_path`; this field lets CI and
    /// `gate-check` find the report without negotiating paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd_report_path: Option<String>,
    /// Structured root-cause candidates emitted when the gate fails or
    /// `evidence_missing` is non-empty.  Each hint pairs a kebab-case
    /// `code` (machine-pattern-matchable) with the specific
    /// `target_selector` it applies to, the `observed` state that
    /// triggered the hint, and a `suggested_probe` (the exact next tool
    /// call the agent should run to confirm the candidate).
    #[serde(default)]
    pub diagnosis_hints: Vec<DiagnosisHint>,
}

/// One root-cause candidate produced by the Animation Evidence Gate.
/// The list is heuristic — multiple hints may apply to the same failure,
/// and the agent is expected to confirm with `suggested_probe` before
/// editing code.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DiagnosisHint {
    /// Stable kebab-case identifier. Agents / CI pattern-match on this.
    /// Defined codes:
    /// - `expected-targets-all-missing` — every expected_target absent from every layout
    /// - `selector-not-found` — a specific expected_target absent from the final layout
    /// - `hidden-by-zero-opacity` — element exists, opacity stayed 0 throughout
    /// - `hidden-by-display-none` — element resolved but `display: none` / zero bbox
    /// - `bbox-static-style-mutating` — bbox unchanged but color/opacity changed
    /// - `intent-target-parent-of-moving-element` — declared parent is static, children move
    /// - `viewport-covered-by-overlay` — a non-shell element covers >=85% of viewport
    /// - `no-dom-mutation-after-trigger` — trigger fired but the page never repainted
    /// - `no-motion-source-registered` — `timeline.sources` is empty
    /// - `motion-source-inactive` — sources exist but none were active at sample time
    /// - `raf-source-stalled` — rAF is running but intent targets never moved
    /// - `css-transition-jumped-over` — CSS transition settled in one step (samples too sparse)
    /// - `css-animation-delay-overrun` — animation active window outside sampled window
    /// - `silent-static-prominent-element` — undeclared prominent element never moved
    /// - `intent-duration-mismatch` — observed span doesn't match `expected_duration_ms`
    /// - `forbidden-kind-observed` — `forbidden_kinds` was seen in the sequence
    /// - `jank-spike` — single transition's pixel_delta is `>= mean + 2σ`
    pub code: String,
    /// Selector the hint is about. `None` for whole-sequence hints
    /// (`no-motion-source-registered`, `no-dom-mutation-after-trigger`).
    /// Always serialized (as `null`) so consumers can pattern-match
    /// `target_selector === null` without checking key existence.
    #[serde(default)]
    pub target_selector: Option<String>,
    /// Short human-readable line — surface verbatim in a PR comment.
    pub message: String,
    /// Structured observation that justified the hint (key/value pairs:
    /// e.g. `{"opacity_first": 0.0, "opacity_last": 0.0}`).
    #[serde(default)]
    pub observed: serde_json::Value,
    /// The exact next tool call to run to confirm the candidate. Either
    /// a fully-formed MCP invocation summary
    /// (`"frame.dom_query { selector: '#hero', expression: '...' }"`)
    /// or `None` when no further probe disambiguates the hint.
    #[serde(default)]
    pub suggested_probe: Option<String>,
}

// === motion.suggest_intent ===

/// Input to `motion.suggest_intent`. Runs a short capture series at `url`
/// (optionally after a trigger), classifies what moved, and returns an
/// `EpisodeIntent` draft the agent can paste into a `MotionContract`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MotionSuggestIntentRequest {
    pub url: String,
    #[serde(default)]
    pub viewport: MotionViewport,
    /// Optional triggers to fire before / during the probe. Uses the same
    /// merged-scheduler semantics as `motion.verify` — when two events
    /// share `at_t_ms`, the trigger fires first.  Omit for animations
    /// that auto-run on load.
    #[serde(default)]
    pub triggers: Vec<ContractTrigger>,
    /// Total virtual-time span to probe, in ms.  Defaults to 480ms —
    /// long enough to cover typical UI transitions without spending a
    /// session.  Minimum 40ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_window_ms: Option<f64>,
    /// Number of evenly-spaced samples within the probe window.  Defaults
    /// to 6 (`[0, w/5, 2w/5, 3w/5, 4w/5, w]`).  Minimum 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_steps: Option<u32>,
    /// Optional selector filter forwarded to the layout probe.  When the
    /// caller already suspects which elements move, this keeps the
    /// `moved_selectors` list focused.  When `None`, every element with
    /// a stable selector is considered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_selectors: Option<Vec<String>>,
}

/// Output of `motion.suggest_intent`.  The `suggested_intent` is advisory:
/// the agent is expected to refine `description`, drop selectors it doesn't
/// own, and tighten `forbidden_kinds` before declaring a final contract.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MotionSuggestIntentResponse {
    /// `EpisodeIntent` draft.  Paste into `MotionContract.episode_intent`.
    pub suggested_intent: EpisodeIntent,
    /// Last sampled time minus first sampled time, in virtual ms.
    pub observed_coverage_ms: f64,
    /// Motion kinds detected across the probe (copied into
    /// `suggested_intent.expected_kinds`).
    pub detected_motion_kinds: Vec<MotionCategory>,
    /// Selectors observed moving across at least one adjacent pair
    /// (copied into `suggested_intent.expected_targets`).
    pub moved_selectors: Vec<String>,
    /// Smoothness over the probe window.  Useful to decide whether the
    /// probe captured the animation cleanly or whether a denser
    /// re-probe is warranted.
    pub smoothness: f64,
    /// `true` when no per-frame pixel change was detected — the suggested
    /// intent will be conservative (empty kinds / targets, descriptive
    /// "no motion observed" text).  Common when the trigger / URL is
    /// wrong, the probe window is too short, or the animation depends on
    /// scroll rather than time.
    pub is_static: bool,
    /// Notes the agent can read directly — coverage / settle time hints,
    /// "no motion detected" warnings, "expand probe_window_ms" suggestions.
    pub notes: Vec<String>,
    /// `document.title` of the page actually served at `url`. Surfaced
    /// at the suggest_intent stage so a port collision / login wall /
    /// redirect is caught before the agent constructs a contract from
    /// the wrong page's draft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_title: Option<String>,
    /// `location.href` of the page actually served at `url`. Pair with
    /// `observed_title` to confirm the page is the one the agent
    /// intended to probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_url: Option<String>,
}
