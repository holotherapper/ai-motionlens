//! ai-motionlens-mcp
//!
//! Stdio MCP server exposing ai-motionlens-core to AI coding agents.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result as AnyResult;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{
    ErrorCode, ErrorData, ListResourcesResult, PaginatedRequestParams, ReadResourceRequestParams,
    ReadResourceResult, ResourceContents, ServerCapabilities, ServerInfo,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::stdio;
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};

use ai_motionlens_core::{
    run_motion_suggest_intent, run_motion_verify, AdvanceOutcome, AnimationAssessment,
    BisectResult, CaptureSeriesResult, ClockStatus, ContactSheet, DomQueryResult, EasingHint,
    EpisodeId, EpisodeIntent, EpisodeStartState, EvidenceLedger, Frame, FrameId, ImageFormat,
    InputMode, Interval, LaunchOptions, LayoutAnomalyKind, LayoutProbeOptions, LayoutSnapshot,
    MotionAuditResult, MotionCategory, MotionContract, MotionDiff, MotionSuggestIntentRequest,
    MotionSuggestIntentResponse, MotionVerifyReport, ObservationId, RecipeInclude,
    ScrollAnimationCheckResult, ScrollSeriesResult, SectionProgressSpec, Session,
    SessionCapabilities, SessionId, TargetSpec, TimelineSources, TriggerEventId, TriggerKind,
    VideoExport, VideoFormat, WaitPolicy,
};

/// Per-session handle. Wrapping each `Session` in its own `Mutex` lets the
/// server hold a HashMap lock only long enough to clone an `Arc`; the
/// long-running browser operation (`capture_frame`, `replay_to`, etc.) then
/// runs under a session-local lock and does not block other sessions.
type SessionHandle = Arc<Mutex<Session>>;
type SessionMap = Arc<Mutex<HashMap<SessionId, SessionHandle>>>;

#[derive(Clone)]
struct MotionLensServer {
    // Populated and read by the #[tool_router] / #[tool_handler] macros;
    // never accessed directly.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
    sessions: SessionMap,
}

impl MotionLensServer {
    /// Look up a session by id and return a clone of its `Arc<Mutex<Session>>`.
    /// The HashMap lock is dropped before the caller awaits any browser work.
    async fn session_handle(&self, id: &SessionId) -> Result<SessionHandle, ErrorData> {
        let map = self.sessions.lock().await;
        map.get(id)
            .cloned()
            .ok_or_else(|| err_invalid(format!("session not found: {id}")))
    }
}

// === Schemas ===

#[derive(Debug, Default, Serialize, JsonSchema)]
struct EmptyOk {}

#[derive(Debug, Deserialize, JsonSchema)]
struct LaunchArgs {
    url: String,
    #[serde(default)]
    viewport_width: Option<u32>,
    #[serde(default)]
    viewport_height: Option<u32>,
    #[serde(default)]
    headless: Option<bool>,
    #[serde(default)]
    device_scale_factor: Option<f32>,
    /// Optional determinism policy. `None` keeps every external source
    /// live. When set, the per-source modes are applied at launch:
    /// random / crypto pin to a seeded PRNG, storage pins to an empty
    /// state, timezone / locale / user_agent override to fixed values.
    #[serde(default)]
    external_state: Option<ai_motionlens_core::ExternalStatePolicy>,
    /// Seed for `random.Pinned`. Ignored unless the policy pins random.
    #[serde(default)]
    external_state_seed: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct LaunchOutput {
    session_id: SessionId,
    /// The page's `document.title` observed right after navigation. The
    /// agent compares this against the page they expected to load —
    /// when a port collision (a stale dev server already listening on
    /// the requested port) silently serves a different page, this is
    /// the fastest early-warning signal. Falls back to `None` when the
    /// page hasn't reached a state where the title is readable yet
    /// (very rare; only on errored navigations).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed_title: Option<String>,
    /// The page's `location.href` observed right after navigation.
    /// Use this alongside `observed_title` to confirm the URL is what
    /// you intended — a redirect, a login wall, or a port collision
    /// will surface here before the agent fires `motion.suggest_intent`
    /// or `motion.verify`, instead of only at verify time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed_url: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SessionRef {
    session_id: SessionId,
}

#[derive(Debug, Serialize, JsonSchema)]
struct EpisodeStartOutput {
    episode_id: EpisodeId,
    start_state: EpisodeStartState,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ReplayToArgs {
    session_id: SessionId,
    episode_id: EpisodeId,
    t_ms: f64,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ReplayToOutput {
    virtual_now_ms: f64,
    replayed_triggers: u32,
    /// The captured scratch frame at `virtual_now_ms`. Persisted into the
    /// episode's ledger so it can be passed to `motion.diff` / `motion.assess`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    frame_id: Option<FrameId>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ClockAdvanceArgs {
    session_id: SessionId,
    delta_ms: f64,
    #[serde(default)]
    max_starvation_count: Option<u32>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ClockAdvanceOutput {
    virtual_now_ms: f64,
    outcome: AdvanceOutcome,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ClickArgs {
    session_id: SessionId,
    selector: String,
    #[serde(default)]
    frame_path: Vec<String>,
    #[serde(default)]
    wait_policy: Option<WaitPolicy>,
    /// `"js"` (default) dispatches via DOM `el.click()`. `"cdp"` fires a
    /// real compositor click via `Input.dispatchMouseEvent`, required
    /// for `:hover` / `pointer*` listeners; the virtual clock advances
    /// ~16ms after dispatch to flush the paint pipeline.
    #[serde(default)]
    input_mode: Option<InputMode>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct HoverArgs {
    session_id: SessionId,
    selector: String,
    #[serde(default)]
    frame_path: Vec<String>,
    #[serde(default)]
    wait_policy: Option<WaitPolicy>,
    /// Same semantics as `ClickArgs.input_mode`. `"cdp"` is required to
    /// activate the CSS `:hover` pseudo-class on the target.
    #[serde(default)]
    input_mode: Option<InputMode>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct TypeArgs {
    session_id: SessionId,
    selector: String,
    text: String,
    #[serde(default)]
    frame_path: Vec<String>,
    #[serde(default)]
    wait_policy: Option<WaitPolicy>,
    /// Same semantics as `ClickArgs.input_mode`. Currently `Type`
    /// always uses the JS path regardless — CDP `Input.insertText`
    /// requires the element to already have focus and the JS path is
    /// structurally equivalent for the observable side effects.
    #[serde(default)]
    input_mode: Option<InputMode>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ScrollArgs {
    session_id: SessionId,
    #[serde(default)]
    selector: Option<String>,
    #[serde(default)]
    frame_path: Vec<String>,
    x: f64,
    y: f64,
    #[serde(default)]
    wait_policy: Option<WaitPolicy>,
    #[serde(default)]
    input_mode: Option<InputMode>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct EvaluateArgs {
    session_id: SessionId,
    js: String,
    #[serde(default)]
    frame_path: Vec<String>,
    #[serde(default)]
    wait_policy: Option<WaitPolicy>,
    #[serde(default)]
    input_mode: Option<InputMode>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct TriggerOutput {
    trigger_id: TriggerEventId,
}

#[derive(Debug, Deserialize, JsonSchema, Default, Clone)]
struct LayoutProbeArgs {
    /// CSS selectors to filter `elements` to (anomalies are still computed
    /// over the whole document).
    #[serde(default)]
    selectors: Option<Vec<String>>,
    /// CSS selectors whose elements should be skipped during anomaly detection
    /// (use for intentional `overflow: hidden` regions like SplitText).
    #[serde(default)]
    ignore_selectors: Option<Vec<String>>,
    /// CSS selectors evaluated against every element via real
    /// `Element.matches()`; each element reports the ones it satisfies in
    /// `matched_selectors`. Lets callers grade arbitrary valid CSS
    /// selectors instead of the synthesized `#id`-priority notation.
    #[serde(default)]
    match_selectors: Option<Vec<String>>,
    /// Anomaly kinds to suppress entirely.
    #[serde(default)]
    ignore_anomalies: Option<Vec<LayoutAnomalyKind>>,
    /// Soft cap on `elements` size. Default 300.
    #[serde(default)]
    max_elements: Option<u32>,
}

impl From<LayoutProbeArgs> for LayoutProbeOptions {
    fn from(a: LayoutProbeArgs) -> Self {
        Self {
            selectors: a.selectors,
            ignore_selectors: a.ignore_selectors,
            match_selectors: a.match_selectors,
            ignore_anomalies: a.ignore_anomalies,
            max_elements: a.max_elements,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CaptureArgs {
    session_id: SessionId,
    #[serde(default)]
    format: Option<ImageFormat>,
    #[serde(default)]
    full_page: Option<bool>,
    /// When set, include a structural `layout_snapshot`. Pass an empty object
    /// `{}` to enable with defaults, or specify `selectors` /
    /// `ignore_selectors` / `ignore_anomalies` / `max_elements`.
    #[serde(default)]
    layout: Option<LayoutProbeArgs>,
    /// Include a thumbnail (base64) in the response. Default: `false`. The
    /// agent should normally read the full image via `artifact_local_path`.
    #[serde(default)]
    thumbnail: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CaptureSeriesArgs {
    session_id: SessionId,
    target_times_ms: Vec<f64>,
    #[serde(default)]
    format: Option<ImageFormat>,
    #[serde(default)]
    full_page: Option<bool>,
    #[serde(default)]
    layout: Option<LayoutProbeArgs>,
    #[serde(default)]
    thumbnail: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LayoutProbeArgsToplevel {
    session_id: SessionId,
    #[serde(default, flatten)]
    options: LayoutProbeArgs,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DomQueryArgs {
    session_id: SessionId,
    /// JavaScript expression to evaluate. The result is JSON-serialized and
    /// returned. Use this for `getComputedStyle(...)`, `getBoundingClientRect`,
    /// or any read-only DOM inspection. NOT recorded into the episode.
    js: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct BisectArgs {
    session_id: SessionId,
    episode_id: EpisodeId,
    interval: Interval,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct EvidenceListArgs {
    session_id: SessionId,
    episode_id: EpisodeId,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AppendObservationArgs {
    session_id: SessionId,
    episode_id: EpisodeId,
    #[serde(default)]
    interval: Option<Interval>,
    claim: String,
    confidence: f64,
    evidence_frame_ids: Vec<FrameId>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ObservationOutput {
    observation_id: ObservationId,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MotionDiffArgs {
    session_id: SessionId,
    frame_id_a: FrameId,
    frame_id_b: FrameId,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ContactSheetArgs {
    session_id: SessionId,
    frame_ids: Vec<FrameId>,
    /// Width per thumbnail in pixels. Default 320.
    #[serde(default)]
    thumb_width: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct VideoExportArgs {
    session_id: SessionId,
    frame_ids: Vec<FrameId>,
    /// Output format: `gif` (default, universal, lossy palette) or `apng`
    /// (lossless, larger files; falls back to a single PNG if APNG
    /// encoding is unavailable).
    #[serde(default)]
    format: Option<VideoFormat>,
    /// Frames per second (1..60). Default 12.
    #[serde(default)]
    fps: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ScrollSeriesArgs {
    session_id: SessionId,
    /// Scroll-progress values in [0..1], ascending.
    progresses: Vec<f64>,
    #[serde(default)]
    format: Option<ImageFormat>,
    #[serde(default)]
    layout: Option<LayoutProbeArgs>,
    #[serde(default)]
    thumbnail: Option<bool>,
    /// Virtual ms to advance after each scroll, letting scroll-driven
    /// animations settle. Default 50ms.
    #[serde(default)]
    settle_ms: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ScrollAnimationCheckArgs {
    session_id: SessionId,
    /// Document-level scroll progress values in `[0..1]`. Mutually exclusive
    /// with `progresses_in_section` (one of the two must be provided).
    #[serde(default)]
    progresses: Option<Vec<f64>>,
    /// Section-relative progress: `{ selector, values }`. Values in `[0..1]`
    /// are translated to document-level progress based on when the section
    /// enters / leaves the viewport. Prefer this for scrub / pin / parallax
    /// when the animation lives in one section of a long page.
    #[serde(default)]
    progresses_in_section: Option<SectionProgressSpec>,
    /// Optional CSS selectors to focus layout snapshots on (e.g. the hero element).
    #[serde(default)]
    target_selectors: Option<Vec<String>>,
    /// Virtual ms to advance after each scroll. Default 50ms.
    #[serde(default)]
    settle_ms: Option<f64>,
    /// Which parts of the result to return. Default `["series", "contact-sheet", "assessment"]`.
    #[serde(default)]
    include: Option<Vec<RecipeInclude>>,
    /// When `true`, `series.frames[]` will carry `thumbnail_base64` and
    /// `layout_snapshot`. Default `false` to keep responses small; rely on
    /// `contact_sheet.artifact_local_path` for visual overview and
    /// `assessment` for scores.
    #[serde(default)]
    include_frame_detail: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SetIntentArgs {
    session_id: SessionId,
    description: String,
    /// Expected duration in milliseconds for TIME-driven animations.
    #[serde(default)]
    expected_duration_ms: Option<f64>,
    /// Expected scroll-progress range `[start, end]` in `[0..1]` for
    /// SCROLL-driven animations (use INSTEAD of expected_duration_ms for
    /// scrub / pin / parallax).
    #[serde(default)]
    expected_progress_range: Option<[f64; 2]>,
    #[serde(default)]
    expected_kinds: Vec<MotionCategory>,
    #[serde(default)]
    expected_targets: Vec<String>,
    #[serde(default)]
    forbidden_kinds: Vec<MotionCategory>,
    /// Expected easing template. Compared against the majority-vote
    /// best-fit easing across `per_target_easing`. Drives
    /// `intent_match.easing_match`.
    #[serde(default)]
    expected_easing: Option<EasingHint>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MotionAssessArgs {
    session_id: SessionId,
    /// Ordered frame IDs (ascending t_ms, same episode).
    frame_ids: Vec<FrameId>,
}

// === Helpers ===

fn err_internal(msg: impl Into<String>) -> ErrorData {
    ErrorData::new(ErrorCode::INTERNAL_ERROR, msg.into(), None)
}

fn err_invalid(msg: impl Into<String>) -> ErrorData {
    ErrorData::new(ErrorCode::INVALID_PARAMS, msg.into(), None)
}

fn to_mcp(e: ai_motionlens_core::Error) -> ErrorData {
    match e {
        ai_motionlens_core::Error::InvalidArgument(_)
        | ai_motionlens_core::Error::BackwardSeekUnsupported { .. } => err_invalid(e.to_string()),
        _ => err_internal(e.to_string()),
    }
}

fn target_spec(selector: String, frame_path: Vec<String>) -> TargetSpec {
    TargetSpec {
        selector,
        frame_path,
        resolved_coordinates: None,
    }
}

// === Server impl ===

#[tool_router]
impl MotionLensServer {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    // -- session --

    #[tool(
        name = "session.launch",
        description = "Launch a Chromium instance, navigate to the URL, freeze the virtual clock at t=0, install preload instrumentation, return a session_id."
    )]
    async fn session_launch(
        &self,
        Parameters(args): Parameters<LaunchArgs>,
    ) -> Result<Json<LaunchOutput>, ErrorData> {
        let opts = LaunchOptions {
            url: args.url,
            viewport_width: args.viewport_width.unwrap_or(1280),
            viewport_height: args.viewport_height.unwrap_or(800),
            headless: args.headless.unwrap_or(true),
            device_scale_factor: args.device_scale_factor.unwrap_or(1.0),
            external_state: args.external_state,
            external_state_seed: args.external_state_seed.unwrap_or(0),
        };
        let session = Session::launch(opts).await.map_err(to_mcp)?;
        let id = session.id.clone();
        // Read title / URL while the session is still owned locally —
        // they're cheap (one `Runtime.evaluate` round-trip) and let the
        // agent catch a port collision / unexpected page before they
        // even reach `motion.suggest_intent`.
        let (observed_title, observed_url) = match session.observed_page_identity().await {
            Ok((t, u)) => (Some(t), Some(u)),
            Err(_) => (None, None),
        };
        self.sessions
            .lock()
            .await
            .insert(id.clone(), Arc::new(Mutex::new(session)));
        Ok(Json(LaunchOutput {
            session_id: id,
            observed_title,
            observed_url,
        }))
    }

    #[tool(
        name = "session.close",
        description = "Close the Chromium instance associated with session_id."
    )]
    async fn session_close(
        &self,
        Parameters(args): Parameters<SessionRef>,
    ) -> Result<Json<EmptyOk>, ErrorData> {
        let handle = self
            .sessions
            .lock()
            .await
            .remove(&args.session_id)
            .ok_or_else(|| err_invalid(format!("session not found: {}", args.session_id)))?;
        let mut s = handle.lock().await;
        s.close().await.map_err(to_mcp)?;
        Ok(Json(EmptyOk {}))
    }

    #[tool(
        name = "session.capabilities",
        description = "Report what the session's driver supports."
    )]
    async fn session_capabilities(
        &self,
        Parameters(args): Parameters<SessionRef>,
    ) -> Result<Json<SessionCapabilities>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let s = handle.lock().await;
        Ok(Json(s.capabilities()))
    }

    // -- episode + clock --

    #[tool(
        name = "episode.start",
        description = "Mark the current page state as t=0 of a new episode."
    )]
    async fn episode_start(
        &self,
        Parameters(args): Parameters<SessionRef>,
    ) -> Result<Json<EpisodeStartOutput>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let mut s = handle.lock().await;
        let (episode_id, start_state) = s.start_episode().map_err(to_mcp)?;
        Ok(Json(EpisodeStartOutput {
            episode_id,
            start_state,
        }))
    }

    #[tool(
        name = "episode.replay_to",
        description = "Replay the recorded episode on a scratch page (the live page is unchanged) and drive the scratch virtual clock to t_ms. The captured scratch frame is pushed to the episode ledger and its `frame_id` is returned so downstream `motion.diff` / `motion.assess` can use it."
    )]
    async fn episode_replay_to(
        &self,
        Parameters(args): Parameters<ReplayToArgs>,
    ) -> Result<Json<ReplayToOutput>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let mut s = handle.lock().await;
        let r = s
            .replay_to(&args.episode_id, args.t_ms)
            .await
            .map_err(to_mcp)?;
        Ok(Json(ReplayToOutput {
            virtual_now_ms: r.virtual_now_ms,
            replayed_triggers: r.replayed_triggers,
            frame_id: r.frame_id,
        }))
    }

    #[tool(
        name = "clock.advance",
        description = "Advance the virtual clock by delta_ms on the live page (forward only)."
    )]
    async fn clock_advance(
        &self,
        Parameters(args): Parameters<ClockAdvanceArgs>,
    ) -> Result<Json<ClockAdvanceOutput>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let mut s = handle.lock().await;
        let r = s
            .clock_advance(args.delta_ms, args.max_starvation_count)
            .await
            .map_err(to_mcp)?;
        Ok(Json(ClockAdvanceOutput {
            virtual_now_ms: r.virtual_now_ms,
            outcome: r.outcome,
        }))
    }

    #[tool(
        name = "clock.status",
        description = "Return current virtual time, policy, and active episode."
    )]
    async fn clock_status(
        &self,
        Parameters(args): Parameters<SessionRef>,
    ) -> Result<Json<ClockStatus>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let s = handle.lock().await;
        let status = s.clock_status().await.map_err(to_mcp)?;
        Ok(Json(status))
    }

    // -- triggers --

    #[tool(
        name = "trigger.click",
        description = "Dispatch a click on the matched element at the current virtual time. Recorded into the active episode at `at_t_ms = clock.virtual_now`; does NOT consume virtual time. Handler body runs synchronously; any rAF / transition / WAAPI animation it kicks off only advances on the next `clock.advance` or `frame.capture_series`."
    )]
    async fn trigger_click(
        &self,
        Parameters(args): Parameters<ClickArgs>,
    ) -> Result<Json<TriggerOutput>, ErrorData> {
        self.dispatch_trigger(
            args.session_id,
            TriggerKind::Click {
                target: target_spec(args.selector, args.frame_path),
            },
            args.wait_policy.unwrap_or_default(),
            args.input_mode.unwrap_or_default(),
        )
        .await
    }

    #[tool(
        name = "trigger.hover",
        description = "Dispatch pointer hover events on the matched element at the current virtual time. Recorded into the active episode; does not consume virtual time."
    )]
    async fn trigger_hover(
        &self,
        Parameters(args): Parameters<HoverArgs>,
    ) -> Result<Json<TriggerOutput>, ErrorData> {
        self.dispatch_trigger(
            args.session_id,
            TriggerKind::Hover {
                target: target_spec(args.selector, args.frame_path),
            },
            args.wait_policy.unwrap_or_default(),
            args.input_mode.unwrap_or_default(),
        )
        .await
    }

    #[tool(
        name = "trigger.type",
        description = "Focus the matched element and type `text` at the current virtual time. Recorded into the active episode; does not consume virtual time."
    )]
    async fn trigger_type(
        &self,
        Parameters(args): Parameters<TypeArgs>,
    ) -> Result<Json<TriggerOutput>, ErrorData> {
        self.dispatch_trigger(
            args.session_id,
            TriggerKind::Type {
                target: target_spec(args.selector, args.frame_path),
                text: args.text,
            },
            args.wait_policy.unwrap_or_default(),
            args.input_mode.unwrap_or_default(),
        )
        .await
    }

    #[tool(
        name = "trigger.scroll",
        description = "Scroll the page (or a scrollable element when `selector` is given) to (x, y) at the current virtual time. Drives scroll-driven animations. Recorded into the active episode; does not consume virtual time."
    )]
    async fn trigger_scroll(
        &self,
        Parameters(args): Parameters<ScrollArgs>,
    ) -> Result<Json<TriggerOutput>, ErrorData> {
        let target = args
            .selector
            .map(|sel| target_spec(sel, args.frame_path.clone()));
        self.dispatch_trigger(
            args.session_id,
            TriggerKind::Scroll {
                target,
                frame_path: args.frame_path,
                x: args.x,
                y: args.y,
            },
            args.wait_policy.unwrap_or_default(),
            args.input_mode.unwrap_or_default(),
        )
        .await
    }

    #[tool(
        name = "trigger.evaluate",
        description = "Run JavaScript in the page (or a specific iframe via `frame_path`). Recorded into the active episode so scratch replays can re-execute it. For read-only state probes that should NOT be recorded, use `frame.dom_query` instead."
    )]
    async fn trigger_evaluate(
        &self,
        Parameters(args): Parameters<EvaluateArgs>,
    ) -> Result<Json<TriggerOutput>, ErrorData> {
        self.dispatch_trigger(
            args.session_id,
            TriggerKind::Evaluate {
                js: args.js,
                frame_path: args.frame_path,
            },
            args.wait_policy.unwrap_or_default(),
            args.input_mode.unwrap_or_default(),
        )
        .await
    }

    // -- frame capture --

    #[tool(
        name = "frame.capture",
        description = "Capture a single frame at the current virtual time. Returns a Frame with artifact_uri (`motionlens://artifacts/...`), thumbnail_base64, dimensions, active_sources."
    )]
    async fn frame_capture(
        &self,
        Parameters(args): Parameters<CaptureArgs>,
    ) -> Result<Json<Frame>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let mut s = handle.lock().await;
        let layout_opts: Option<LayoutProbeOptions> = args.layout.map(Into::into);
        let f = s
            .capture_frame(
                args.format.unwrap_or_default(),
                args.full_page.unwrap_or(false),
                layout_opts.as_ref(),
                args.thumbnail.unwrap_or(false),
            )
            .await
            .map_err(to_mcp)?;
        Ok(Json(f))
    }

    #[tool(
        name = "frame.capture_series",
        description = "Advance the virtual clock to each absolute target in target_times_ms[] (ascending) and capture a frame at each point. Returns frames + sampled_points + unobserved_intervals."
    )]
    async fn frame_capture_series(
        &self,
        Parameters(args): Parameters<CaptureSeriesArgs>,
    ) -> Result<Json<CaptureSeriesResult>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let mut s = handle.lock().await;
        let layout_opts = args.layout.map(Into::into);
        let r = s
            .capture_series(
                &args.target_times_ms,
                args.format.unwrap_or_default(),
                args.full_page.unwrap_or(false),
                layout_opts,
                args.thumbnail.unwrap_or(false),
            )
            .await
            .map_err(to_mcp)?;
        Ok(Json(r))
    }

    #[tool(
        name = "frame.layout_probe",
        description = "Take a structured layout snapshot at the current virtual time WITHOUT saving an artifact image. Supports `selectors` to filter the elements array, `ignore_selectors` to skip intentional clipping regions during anomaly detection, `ignore_anomalies` to suppress entire kinds, and `max_elements` to override the 300 element cap. Returns viewport, document scroll size, visible elements (bbox + computed style essentials), and anomalies: horizontal-viewport-overflow, vertical-viewport-overflow, content-clipped, off-screen, text-ellipsis-truncated, overlap-stacking. Also returns `truncated` if elements were dropped."
    )]
    async fn frame_layout_probe(
        &self,
        Parameters(args): Parameters<LayoutProbeArgsToplevel>,
    ) -> Result<Json<LayoutSnapshot>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let s = handle.lock().await;
        let snap = s.layout_probe(&args.options.into()).await.map_err(to_mcp)?;
        Ok(Json(snap))
    }

    #[tool(
        name = "frame.dom_query",
        description = "Run a read-only JavaScript expression in the page and return its JSON-serialized value (with elapsed_ms). Use for getComputedStyle(...), element.getBoundingClientRect(), window.scrollY, accessing window.gsap / window.lenis state, anything where the agent needs an exact value back. NOT recorded into the episode - if you want replay-safe state changes use trigger.evaluate instead."
    )]
    async fn frame_dom_query(
        &self,
        Parameters(args): Parameters<DomQueryArgs>,
    ) -> Result<Json<DomQueryResult>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let s = handle.lock().await;
        let r = s.dom_query(&args.js).await.map_err(to_mcp)?;
        Ok(Json(r))
    }

    #[tool(
        name = "library.gsap_state",
        description = "Read GSAP runtime state: master timeline progress + currentTime, all ScrollTrigger instances with their progress/isActive/start/end. Returns `{ available, master, scroll_triggers }`. If `available: false`, GSAP isn't loaded on the page."
    )]
    async fn library_gsap_state(
        &self,
        Parameters(args): Parameters<SessionRef>,
    ) -> Result<Json<DomQueryResult>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let s = handle.lock().await;
        let js = r#"(function(){
  const out = { available: typeof window.gsap !== 'undefined', master: null, scroll_triggers: [] };
  if (!out.available) return out;
  const gsap = window.gsap;
  try {
    const tl = gsap.globalTimeline;
    out.master = { time: tl.time(), progress: tl.progress(), totalDuration: tl.totalDuration(), paused: tl.paused() };
  } catch (e) { out.master = { error: String(e && e.message || e) }; }
  if (typeof ScrollTrigger !== 'undefined') {
    try {
      out.scroll_triggers = ScrollTrigger.getAll().map(t => ({
        id: (t.vars && t.vars.id) || (t.trigger && t.trigger.id) || null,
        progress: t.progress,
        isActive: t.isActive,
        start: t.start,
        end: t.end,
        scrub: !!(t.vars && t.vars.scrub),
        pin: !!(t.vars && t.vars.pin),
      }));
    } catch (e) { out.scroll_triggers = [{ error: String(e && e.message || e) }]; }
  }
  return out;
})()"#;
        let r = s.dom_query(js).await.map_err(to_mcp)?;
        Ok(Json(r))
    }

    #[tool(
        name = "library.lenis_state",
        description = "Read Lenis smooth-scroll state: current smooth-scroll position, native scrollY, velocity, isStopped. Returns `{ available, scroll, velocity, native_scroll_y, is_stopped }`. If Lenis isn't attached as `window.lenis`, `available: false`."
    )]
    async fn library_lenis_state(
        &self,
        Parameters(args): Parameters<SessionRef>,
    ) -> Result<Json<DomQueryResult>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let s = handle.lock().await;
        let js = r#"(function(){
  const l = window.lenis;
  if (!l) return { available: false, native_scroll_y: window.scrollY };
  return {
    available: true,
    scroll: l.scroll,
    velocity: l.velocity,
    is_stopped: !!l.isStopped,
    native_scroll_y: window.scrollY,
    limit: l.limit,
  };
})()"#;
        let r = s.dom_query(js).await.map_err(to_mcp)?;
        Ok(Json(r))
    }

    #[tool(
        name = "library.lottie_state",
        description = "Read Lottie player state for any animations registered with bodymovin / lottie-web. Returns `{ available, count, players: [{ currentFrame, totalFrames, isPaused, isLoaded, name }] }`."
    )]
    async fn library_lottie_state(
        &self,
        Parameters(args): Parameters<SessionRef>,
    ) -> Result<Json<DomQueryResult>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let s = handle.lock().await;
        let js = r#"(function(){
  let lottie = window.lottie || window.bodymovin;
  if (!lottie || typeof lottie.getRegisteredAnimations !== 'function') {
    return { available: false, count: 0, players: [] };
  }
  const all = lottie.getRegisteredAnimations();
  return {
    available: true,
    count: all.length,
    players: all.map(a => ({
      currentFrame: a.currentFrame,
      totalFrames: a.totalFrames,
      isPaused: a.isPaused,
      isLoaded: a.isLoaded,
      name: a.name || null,
    })),
  };
})()"#;
        let r = s.dom_query(js).await.map_err(to_mcp)?;
        Ok(Json(r))
    }

    #[tool(
        name = "frame.bisect",
        description = "Capture the midpoint frame of an unobserved interval. If the midpoint is past the live virtual clock, runs a scratch-page replay (the live page is never rewound). The captured frame is appended to the episode ledger so it can be passed to `motion.diff` / `motion.assess` like any other frame."
    )]
    async fn frame_bisect(
        &self,
        Parameters(args): Parameters<BisectArgs>,
    ) -> Result<Json<BisectResult>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let mut s = handle.lock().await;
        let r = s
            .bisect(&args.episode_id, args.interval)
            .await
            .map_err(to_mcp)?;
        Ok(Json(r))
    }

    // -- timeline --

    #[tool(
        name = "timeline.sources",
        description = "Detect time-based animation sources on the page. Each category carries DetectionMeta (method / confidence / limitations)."
    )]
    async fn timeline_sources(
        &self,
        Parameters(args): Parameters<SessionRef>,
    ) -> Result<Json<TimelineSources>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let s = handle.lock().await;
        let r = s.timeline_sources().await.map_err(to_mcp)?;
        Ok(Json(r))
    }

    #[tool(
        name = "motion.audit_required",
        description = "Decide whether the page has time-dependent UI that requires a `motion.verify` evidence pass before the caller can claim 'verified'. Returns `{ motion_detected, sources, required_verification, recommended_target_times_ms, reason }`. Call this BEFORE you take a Playwright screenshot to decide whether the screenshot would be sufficient evidence (it isn't, if `required_verification: true`). Designed for CI / sub-agent consumption — humans rarely need it directly."
    )]
    async fn motion_audit_required(
        &self,
        Parameters(args): Parameters<SessionRef>,
    ) -> Result<Json<MotionAuditResult>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let s = handle.lock().await;
        let r = s.audit_motion_sources().await.map_err(to_mcp)?;
        Ok(Json(r))
    }

    #[tool(
        name = "motion.verify",
        description = "Animation Evidence Gate. Takes a `MotionContract`, spawns a Chrome session, runs the full verify loop, writes `motionlens-report.json` to the session artifacts directory, and returns a `MotionVerifyReport`. `passes:true` is the single pass/fail bar across smoothness / jank / intent_match / expected_targets / coverage / non-static. The high-level entry point for verifying time-dependent UI — prefer this over orchestrating low-level tools yourself."
    )]
    async fn motion_verify(
        &self,
        Parameters(contract): Parameters<MotionContract>,
    ) -> Result<Json<MotionVerifyReport>, ErrorData> {
        let r = run_motion_verify(contract).await.map_err(to_mcp)?;
        Ok(Json(r))
    }

    #[tool(
        name = "motion.suggest_intent",
        description = "Probe a page and propose an `EpisodeIntent` draft (description, expected_duration_ms, expected_kinds, expected_targets) plus raw observations (detected_motion_kinds, moved_selectors, smoothness, is_static, notes). Use this BEFORE writing a `MotionContract` when the agent doesn't already know what the animation does — paste `suggested_intent` into the contract and refine. The draft is advisory; tighten the description, drop selectors you don't own, and add `forbidden_kinds` before treating it as a contract."
    )]
    async fn motion_suggest_intent(
        &self,
        Parameters(req): Parameters<MotionSuggestIntentRequest>,
    ) -> Result<Json<MotionSuggestIntentResponse>, ErrorData> {
        let r = run_motion_suggest_intent(req).await.map_err(to_mcp)?;
        Ok(Json(r))
    }

    // -- evidence --

    #[tool(
        name = "evidence.list",
        description = "Return everything recorded in the given episode: start_state, triggers, frames, diffs, observations."
    )]
    async fn evidence_list(
        &self,
        Parameters(args): Parameters<EvidenceListArgs>,
    ) -> Result<Json<EvidenceLedger>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let s = handle.lock().await;
        let ledger = s.evidence(&args.episode_id).map_err(to_mcp)?.clone();
        Ok(Json(ledger))
    }

    #[tool(
        name = "evidence.append_observation",
        description = "Append the agent's explicit claim about an interval, with confidence and evidence frame IDs."
    )]
    async fn evidence_append_observation(
        &self,
        Parameters(args): Parameters<AppendObservationArgs>,
    ) -> Result<Json<ObservationOutput>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let mut s = handle.lock().await;
        let id = s
            .append_observation(
                &args.episode_id,
                args.interval,
                args.claim,
                args.confidence,
                args.evidence_frame_ids,
            )
            .map_err(to_mcp)?;
        Ok(Json(ObservationOutput { observation_id: id }))
    }

    // -- motion diff --

    #[tool(
        name = "motion.diff",
        description = "Compute a structured diff between two captured frames."
    )]
    async fn motion_diff(
        &self,
        Parameters(args): Parameters<MotionDiffArgs>,
    ) -> Result<Json<MotionDiff>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let s = handle.lock().await;
        let diff = s
            .motion_diff(&args.frame_id_a, &args.frame_id_b)
            .await
            .map_err(to_mcp)?;
        Ok(Json(diff))
    }

    #[tool(
        name = "motion.video_export",
        description = "Encode an ordered set of captured frames into an animated GIF (or APNG) at the chosen fps. Use this when the agent wants to hand the user a playable artifact for review. Returns artifact_uri + artifact_local_path."
    )]
    async fn motion_video_export(
        &self,
        Parameters(args): Parameters<VideoExportArgs>,
    ) -> Result<Json<VideoExport>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let mut s = handle.lock().await;
        let r = s
            .video_export(
                &args.frame_ids,
                args.format.unwrap_or_default(),
                args.fps.unwrap_or(12.0),
            )
            .await
            .map_err(to_mcp)?;
        Ok(Json(r))
    }

    #[tool(
        name = "motion.contact_sheet",
        description = "Render N captured frames into a single horizontal time-strip PNG (one artifact). Use this to give the agent a one-glance view of the whole animation. Returns artifact_uri + artifact_local_path the agent can Read directly."
    )]
    async fn motion_contact_sheet(
        &self,
        Parameters(args): Parameters<ContactSheetArgs>,
    ) -> Result<Json<ContactSheet>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let mut s = handle.lock().await;
        let sheet = s
            .contact_sheet(&args.frame_ids, args.thumb_width.unwrap_or(320), None)
            .await
            .map_err(to_mcp)?;
        Ok(Json(sheet))
    }

    #[tool(
        name = "scroll.series",
        description = "Drive document scroll to each progress value in [0..1] (ascending), let scroll-driven animations settle for `settle_ms`, and capture a frame at each step. Use this for scroll-driven animations (parallax, pin, scrub). Supports the same `layout` / `thumbnail` options as frame.capture_series."
    )]
    async fn scroll_series(
        &self,
        Parameters(args): Parameters<ScrollSeriesArgs>,
    ) -> Result<Json<ScrollSeriesResult>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let mut s = handle.lock().await;
        let r = s
            .scroll_series(
                &args.progresses,
                args.format.unwrap_or_default(),
                args.layout.map(Into::into),
                args.thumbnail.unwrap_or(false),
                args.settle_ms.unwrap_or(50.0),
            )
            .await
            .map_err(to_mcp)?;
        Ok(Json(r))
    }

    #[tool(
        name = "episode.set_intent",
        description = "Record the agent's Plan (Step 1 of the SKILL loop) into the active episode. Stored fields: description, expected_duration_ms, expected_kinds, expected_targets, forbidden_kinds. Consulted by motion.assess to flag mismatches (missing motion, forbidden motion, duration drift)."
    )]
    async fn episode_set_intent(
        &self,
        Parameters(args): Parameters<SetIntentArgs>,
    ) -> Result<Json<EmptyOk>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let mut s = handle.lock().await;
        let intent = EpisodeIntent {
            description: args.description,
            expected_duration_ms: args.expected_duration_ms,
            expected_progress_range: args.expected_progress_range,
            expected_kinds: args.expected_kinds,
            expected_targets: args.expected_targets,
            forbidden_kinds: args.forbidden_kinds,
            expected_easing: args.expected_easing,
        };
        s.set_intent(intent).map_err(to_mcp)?;
        Ok(Json(EmptyOk {}))
    }

    #[tool(
        name = "recipes.scroll_animation_check",
        description = "High-level recipe: scroll.series + motion.contact_sheet + motion.assess in one call. Use as the default entry point for scroll-driven animation debugging. Returns series (per-progress frames + scroll positions), contact_sheet (one-glance time-strip artifact), and assessment (smoothness / jank / motion kinds / intent match if set_intent was called)."
    )]
    async fn recipes_scroll_animation_check(
        &self,
        Parameters(args): Parameters<ScrollAnimationCheckArgs>,
    ) -> Result<Json<ScrollAnimationCheckResult>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let mut s = handle.lock().await;
        let progresses = match (args.progresses, args.progresses_in_section) {
            (Some(p), None) => p,
            (None, Some(spec)) => s.resolve_section_progress(&spec).await.map_err(to_mcp)?,
            (Some(_), Some(_)) => {
                return Err(err_invalid(
                    "supply either `progresses` or `progresses_in_section`, not both",
                ))
            }
            (None, None) => {
                return Err(err_invalid(
                    "one of `progresses` or `progresses_in_section` is required",
                ))
            }
        };
        let include = args.include.unwrap_or_else(|| {
            vec![
                RecipeInclude::Series,
                RecipeInclude::ContactSheet,
                RecipeInclude::Assessment,
            ]
        });
        let r = s
            .scroll_animation_check(
                &progresses,
                args.target_selectors,
                args.settle_ms.unwrap_or(50.0),
                &include,
                args.include_frame_detail.unwrap_or(false),
            )
            .await
            .map_err(to_mcp)?;
        Ok(Json(r))
    }

    #[tool(
        name = "motion.assess",
        description = "Score the quality of an animation from an ordered sequence of captured frames. Returns smoothness (0-1), per-transition pixel deltas, jank events, detected_motion_kinds (fade/translate/scale/rotate/color-change/layout/appearance/disappearance from layout snapshots), per_transition_kinds, intent_match (if episode.set_intent was called), summary, recommendations. Use AFTER frame.capture_series with `layout: {}` for richest analysis."
    )]
    async fn motion_assess(
        &self,
        Parameters(args): Parameters<MotionAssessArgs>,
    ) -> Result<Json<AnimationAssessment>, ErrorData> {
        let handle = self.session_handle(&args.session_id).await?;
        let s = handle.lock().await;
        let assess = s.motion_assess(&args.frame_ids).await.map_err(to_mcp)?;
        Ok(Json(assess))
    }
}

impl MotionLensServer {
    async fn dispatch_trigger(
        &self,
        session_id: SessionId,
        kind: TriggerKind,
        wait_policy: WaitPolicy,
        input_mode: InputMode,
    ) -> Result<Json<TriggerOutput>, ErrorData> {
        let handle = self.session_handle(&session_id).await?;
        let mut s = handle.lock().await;
        let trigger_id = s
            .trigger(kind, wait_policy, input_mode)
            .await
            .map_err(to_mcp)?;
        Ok(Json(TriggerOutput { trigger_id }))
    }
}

#[tool_handler]
impl ServerHandler for MotionLensServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        info.instructions = Some(
            "ai-motionlens: virtual-clock driven animation observation for AI agents. \
             Frames returned by capture tools carry `artifact_uri` (motionlens://artifacts/...) \
             and `artifact_local_path`. Read full-resolution images via `resources/read` \
             (uri) or directly from the local path."
                .into(),
        );
        info
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        // Artifacts are surfaced through tool responses (`Frame.artifact_uri`).
        // We deliberately don't enumerate a flat catalog -- callers come back
        // with a concrete URI they already received from a tool call.
        Ok(ListResourcesResult {
            resources: Vec::new(),
            next_cursor: None,
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let path = ai_motionlens_core::artifact::ArtifactStore::resolve_uri(&request.uri)
            .map_err(|e| err_invalid(e.to_string()))?;
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| err_internal(format!("read artifact: {e}")))?;
        let mime = mime_for_path(&path);
        let blob_b64 = BASE64_STANDARD.encode(&bytes);
        Ok(ReadResourceResult::new(vec![ResourceContents::blob(
            blob_b64,
            request.uri,
        )
        .with_mime_type(mime)]))
    }
}

fn mime_for_path(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("apng") => "image/apng",
        _ => "application/octet-stream",
    }
}

#[tokio::main]
async fn main() -> AnyResult<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ai_motionlens=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let service = MotionLensServer::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
