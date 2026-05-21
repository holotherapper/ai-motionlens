//! Time-control backends.
//!
//! [`TimeDriver`] is the contract for **clock** drivers. It is intentionally
//! narrow: a driver only advances/pauses/queries the virtual clock. Observing
//! what animations exist on the page is the job of a separate
//! [`TimelineSourceCollector`].

use std::sync::Arc;

use async_trait::async_trait;
use chromiumoxide::cdp::browser_protocol::animation::Animation as CdpAnimation;
use chromiumoxide::cdp::browser_protocol::animation::AnimationType as CdpAnimationType;
use chromiumoxide::cdp::browser_protocol::emulation::{
    SetVirtualTimePolicyParams, VirtualTimePolicy,
};
use chromiumoxide::Page;
use futures::StreamExt as _;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::error::{Error, Result};
use crate::model::{
    ActiveSource, AdvanceOutcome, AdvanceResult, ClockPolicy, ClockStatus, DetectedAnimation,
    DetectionMeta, DetectionMethod, DriverKind, ExternalStatePolicy, RafDetectionResult,
    ReplayCostModel, SeekBackStrategy, SessionCapabilities, SourceKind, TimelineCategory,
    TimelineSources, TimerCounts, TimerDetectionResult,
};

// === TimeDriver ===

#[async_trait]
pub trait TimeDriver: Send + Sync {
    fn kind(&self) -> DriverKind;
    fn capabilities(&self) -> SessionCapabilities;
    async fn pause(&mut self) -> Result<()>;
    async fn advance(&mut self, delta_ms: f64, max_starvation_count: u32) -> Result<AdvanceResult>;
    async fn status(&self) -> Result<ClockStatus>;
}

fn pause_policy() -> Result<SetVirtualTimePolicyParams> {
    SetVirtualTimePolicyParams::builder()
        .policy(VirtualTimePolicy::Pause)
        .build()
        .map_err(|e| Error::browser(format!("build SetVirtualTimePolicy(Pause): {e}")))
}

/// CDP `Emulation.setVirtualTimePolicy` based driver (primary backend).
pub struct VirtualTimeDriver {
    page: Page,
    state: Arc<Mutex<VirtualTimeState>>,
}

struct VirtualTimeState {
    virtual_now_ms: f64,
    policy: ClockPolicy,
}

impl VirtualTimeDriver {
    /// `initial_now_ms` is the virtual time the driver should report after `pause()`.
    /// Typical callers pass `0.0` right after launching the browser.
    pub fn new(page: Page, initial_now_ms: f64) -> Self {
        Self {
            page,
            state: Arc::new(Mutex::new(VirtualTimeState {
                virtual_now_ms: initial_now_ms,
                policy: ClockPolicy::Pause,
            })),
        }
    }

    /// Initialize the virtual clock to `pause` at the current virtual now.
    pub async fn initialize(&self) -> Result<()> {
        self.page
            .execute(pause_policy()?)
            .await
            .map_err(Error::browser)?;
        // Enable lifecycle events so `wait_network_almost_idle_best_effort`
        // and any user-level `WaitPolicy::UntilNetworkIdle` calls receive the
        // `networkAlmostIdle` signal.
        let lifecycle =
            chromiumoxide::cdp::browser_protocol::page::SetLifecycleEventsEnabledParams::builder()
                .enabled(true)
                .build()
                .map_err(|e| Error::browser(format!("build SetLifecycleEventsEnabled: {e}")))?;
        self.page.execute(lifecycle).await.map_err(Error::browser)?;
        Ok(())
    }
}

#[async_trait]
impl TimeDriver for VirtualTimeDriver {
    fn kind(&self) -> DriverKind {
        DriverKind::VirtualTime
    }

    fn capabilities(&self) -> SessionCapabilities {
        SessionCapabilities {
            driver: DriverKind::VirtualTime,
            can_seek_back: true,
            seek_back_strategy: SeekBackStrategy::ReplayScratch,
            live_clock_forward_only: true,
            supports_css: true,
            supports_waapi: true,
            supports_raf: true,
            supports_scroll_driven: false,
            external_state: ExternalStatePolicy::default(),
            replay_cost: ReplayCostModel {
                replay_fixed_cost_ms: 500.0,
                cost_per_virtual_ms: 0.5,
                cost_per_trigger_ms: 30.0,
                capture_cost_ms: 80.0,
                network_wait_risk: 0.2,
            },
        }
    }

    async fn pause(&mut self) -> Result<()> {
        self.page
            .execute(pause_policy()?)
            .await
            .map_err(Error::browser)?;
        let mut state = self.state.lock().await;
        state.policy = ClockPolicy::Pause;
        Ok(())
    }

    async fn advance(&mut self, delta_ms: f64, max_starvation_count: u32) -> Result<AdvanceResult> {
        if delta_ms < 0.0 {
            return Err(Error::invalid("delta_ms must be >= 0"));
        }
        if delta_ms == 0.0 {
            let state = self.state.lock().await;
            return Ok(AdvanceResult {
                virtual_now_ms: state.virtual_now_ms,
                outcome: AdvanceOutcome::Advanced,
            });
        }

        let mut budget_events = self
            .page
            .event_listener::<chromiumoxide::cdp::browser_protocol::emulation::EventVirtualTimeBudgetExpired>()
            .await
            .map_err(Error::browser)?;

        {
            let mut state = self.state.lock().await;
            state.policy = ClockPolicy::Advance;
        }

        let params = SetVirtualTimePolicyParams::builder()
            .policy(VirtualTimePolicy::Advance)
            .budget(delta_ms)
            .max_virtual_time_task_starvation_count(max_starvation_count as i64)
            .build()
            .map_err(|e| Error::browser(format!("build SetVirtualTimePolicy: {e}")))?;
        self.page.execute(params).await.map_err(Error::browser)?;

        // Wait for the budget to expire. The timeout is proportional to the
        // budget plus a fixed slack so a pathological page can never hang us.
        let timeout_ms = (delta_ms.max(50.0) * 20.0).max(2000.0);
        let budget_outcome = tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms as u64),
            budget_events.next(),
        )
        .await;

        // Always pin the policy back to Pause regardless of outcome so the
        // live clock cannot keep drifting forward on its own.
        let _ = self.page.execute(pause_policy()?).await;

        let (advanced_ms, outcome) = match budget_outcome {
            Ok(Some(_event)) => (delta_ms, AdvanceOutcome::Advanced),
            // Stream closed or timeout: the budget never expired. The clock
            // did not actually move by `delta_ms` -- do NOT lie to the caller.
            Ok(None) | Err(_) => (0.0, AdvanceOutcome::StarvationHit),
        };

        // Network quiescence is not auto-waited here. Agents that care about
        // pending fetches between an advance and a capture should request it
        // explicitly via `WaitPolicy::UntilNetworkIdle` on the next trigger,
        // which honors `networkAlmostIdle` with its own bounded timeout.
        // Auto-waiting inside every `advance` would create CDP listener churn
        // for advance-heavy flows (`capture_series` does N back-to-back
        // advances) without giving the agent control over the trade-off.

        let mut state = self.state.lock().await;
        state.virtual_now_ms += advanced_ms;
        state.policy = ClockPolicy::Pause;
        Ok(AdvanceResult {
            virtual_now_ms: state.virtual_now_ms,
            outcome,
        })
    }

    async fn status(&self) -> Result<ClockStatus> {
        let state = self.state.lock().await;
        Ok(ClockStatus {
            virtual_now_ms: state.virtual_now_ms,
            policy: state.policy,
            current_episode_id: None,
        })
    }
}

// === TimelineSourceCollector ===

#[async_trait]
pub trait TimelineSourceCollector: Send + Sync {
    async fn collect(&self) -> Result<TimelineSources>;
}

pub type AnimationBuffer = Arc<Mutex<Vec<CdpAnimation>>>;

pub struct CdpTimelineCollector {
    page: Page,
    pub preload_instrumentation_installed: bool,
    animations: AnimationBuffer,
}

impl CdpTimelineCollector {
    pub fn new(
        page: Page,
        preload_instrumentation_installed: bool,
        animations: AnimationBuffer,
    ) -> Self {
        Self {
            page,
            preload_instrumentation_installed,
            animations,
        }
    }

    fn raf_detection(&self) -> DetectionMeta {
        if self.preload_instrumentation_installed {
            DetectionMeta {
                method: DetectionMethod::PreloadInstrumentation,
                confidence: 0.9,
                limitations: "Detects rAF callbacks scheduled after preload instrumentation \
                              attached. Misses callbacks scheduled in inline scripts that ran \
                              before instrumentation."
                    .into(),
            }
        } else {
            DetectionMeta {
                method: DetectionMethod::Heuristic,
                confidence: 0.4,
                limitations: "No preload instrumentation. rAF presence can only be guessed from \
                              runtime probes and may be missed entirely."
                    .into(),
            }
        }
    }

    fn timer_detection(&self) -> DetectionMeta {
        if self.preload_instrumentation_installed {
            DetectionMeta {
                method: DetectionMethod::PreloadInstrumentation,
                confidence: 0.85,
                limitations: "setTimeout / setInterval calls are counted after preload hook \
                              attached. Earlier calls are missed."
                    .into(),
            }
        } else {
            DetectionMeta {
                method: DetectionMethod::NotSupported,
                confidence: 0.0,
                limitations: "Timer enumeration requires preload instrumentation; not installed."
                    .into(),
            }
        }
    }

    fn cdp_animation_detection(&self) -> DetectionMeta {
        DetectionMeta {
            method: DetectionMethod::CdpAnimation,
            confidence: 0.95,
            limitations:
                "Authoritative for CSS animations, CSS transitions, and WAAPI animations. Excludes \
                 rAF-only animations and scroll timelines that the agent has not navigated through."
                    .into(),
        }
    }

    fn scroll_driven_detection(&self) -> DetectionMeta {
        DetectionMeta {
            method: DetectionMethod::CdpAnimation,
            confidence: 0.7,
            limitations:
                "Scroll-driven animations appear in CDP Animation events only after the scroll \
                 range is entered. Some may be missed until the agent triggers a scroll."
                    .into(),
        }
    }

    /// Return CDP-known animations that are active at the given virtual time
    /// (i.e. `start_time <= now <= start_time + duration * iterations`, or
    /// infinite iterations). The result is suitable for `Frame.active_sources`.
    pub async fn active_sources_at(&self, virtual_now_ms: f64) -> Vec<ActiveSource> {
        self.animations
            .lock()
            .await
            .iter()
            .filter_map(|a| classify_active(a, virtual_now_ms))
            .collect()
    }
}

fn cdp_name_opt(a: &CdpAnimation) -> Option<String> {
    if a.name.is_empty() {
        None
    } else {
        Some(a.name.clone())
    }
}

fn classify_active(a: &CdpAnimation, virtual_now_ms: f64) -> Option<ActiveSource> {
    let kind = match a.r#type {
        CdpAnimationType::CssAnimation => {
            if a.view_or_scroll_timeline.is_some() {
                SourceKind::ScrollDriven
            } else {
                SourceKind::CssAnimation
            }
        }
        CdpAnimationType::CssTransition => SourceKind::CssTransition,
        CdpAnimationType::WebAnimation => SourceKind::WebAnimation,
    };
    let start_ms = a.start_time;
    let (duration_ms, iterations) = a
        .source
        .as_ref()
        .map(|s| (s.duration, s.iterations.unwrap_or(1.0)))
        .unwrap_or((0.0, 1.0));
    let is_infinite = !iterations.is_finite() || iterations <= 0.0;
    let elapsed = virtual_now_ms - start_ms;
    if elapsed < 0.0 {
        return None;
    }
    let active = if is_infinite {
        true
    } else {
        elapsed <= duration_ms * iterations
    };
    if !active {
        return None;
    }
    let progress = if duration_ms > 0.0 {
        let raw = elapsed / duration_ms;
        Some(if is_infinite {
            raw.fract()
        } else {
            raw.min(1.0)
        })
    } else {
        None
    };
    let total_duration_ms = if is_infinite {
        duration_ms.max(1e-6)
    } else {
        duration_ms * iterations
    };
    Some(ActiveSource {
        id: a.id.clone(),
        kind,
        target_selector: a.css_id.clone(),
        progress,
        name: cdp_name_opt(a),
        duration_ms: total_duration_ms,
        // Resolved later in motion.verify once a layout snapshot is
        // available to cross-reference against `running_animations`.
        target_selector_hint: None,
    })
}

#[async_trait]
impl TimelineSourceCollector for CdpTimelineCollector {
    async fn collect(&self) -> Result<TimelineSources> {
        let cdp = self.cdp_animation_detection();
        let scroll = self.scroll_driven_detection();

        // Animations recorded by the launch-time event listener.
        let (mut css_anim, mut css_trans, mut waapi, mut scroll_driven) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        {
            let animations = self.animations.lock().await;
            for a in animations.iter() {
                let detected = detected_from_cdp(a);
                match a.r#type {
                    CdpAnimationType::CssAnimation => {
                        if a.view_or_scroll_timeline.is_some() {
                            scroll_driven.push(crate::model::ScrollDrivenSource {
                                target_selector: detected.target_selector.clone(),
                                axis: "block".into(),
                                range_description: None,
                            });
                        } else {
                            css_anim.push(detected);
                        }
                    }
                    CdpAnimationType::CssTransition => css_trans.push(detected),
                    CdpAnimationType::WebAnimation => waapi.push(detected),
                }
            }
        }

        // Media + rAF + timer counts via preload instrumentation.
        let probe_js = r#"
            (function () {
                const media = Array.from(document.querySelectorAll('video, audio'))
                    .map(el => el.id ? '#' + el.id : el.tagName.toLowerCase());
                const m = window.__motionlens || {};
                function n(k) { return (m[k] | 0); }
                return JSON.stringify({
                    media,
                    rafCount: n('rafCount'),
                    rafActiveCount: n('rafActiveCount'),
                    rafCancelCount: n('rafCancelCount'),
                    setTimeoutCount: n('setTimeoutCount'),
                    setTimeoutActiveCount: n('setTimeoutActiveCount'),
                    setTimeoutCancelCount: n('setTimeoutCancelCount'),
                    setIntervalCount: n('setIntervalCount'),
                    setIntervalActiveCount: n('setIntervalActiveCount'),
                    setIntervalCancelCount: n('setIntervalCancelCount'),
                });
            })()
        "#;
        let probe: ProbeData = match self.page.evaluate(probe_js).await {
            Ok(result) => {
                let s = result
                    .into_value::<String>()
                    .unwrap_or_else(|_| "{}".into());
                serde_json::from_str(&s).unwrap_or_default()
            }
            Err(_) => ProbeData::default(),
        };

        Ok(TimelineSources {
            css_animations: TimelineCategory {
                items: css_anim,
                detection: cdp.clone(),
            },
            css_transitions: TimelineCategory {
                items: css_trans,
                detection: cdp.clone(),
            },
            waapi: TimelineCategory {
                items: waapi,
                detection: cdp,
            },
            raf: RafDetectionResult {
                detected: probe.raf_total > 0,
                total_count: probe.raf_total,
                active_count: probe.raf_active,
                cancel_count: probe.raf_cancel,
                detection: self.raf_detection(),
            },
            scroll_driven: TimelineCategory {
                items: scroll_driven,
                detection: scroll,
            },
            media_elements: TimelineCategory {
                items: probe.media,
                detection: DetectionMeta {
                    method: DetectionMethod::Heuristic,
                    confidence: 0.6,
                    limitations: "Detected by querySelectorAll('video, audio') at collect time."
                        .into(),
                },
            },
            timers: TimerDetectionResult {
                counts: TimerCounts {
                    set_timeout_count: probe.st_total,
                    set_interval_count: probe.si_total,
                    set_timeout_active: probe.st_active,
                    set_interval_active: probe.si_active,
                    set_timeout_cancel_count: probe.st_cancel,
                    set_interval_cancel_count: probe.si_cancel,
                },
                detection: self.timer_detection(),
            },
        })
    }
}

/// Decoded shape of the `probe_js` payload. Field names map to the JS keys via
/// `serde(rename)`; every field is `default` so a missing key reads as zero.
#[derive(Deserialize, Default)]
struct ProbeData {
    #[serde(default)]
    media: Vec<String>,
    #[serde(rename = "rafCount", default)]
    raf_total: u32,
    #[serde(rename = "rafActiveCount", default)]
    raf_active: u32,
    #[serde(rename = "rafCancelCount", default)]
    raf_cancel: u32,
    #[serde(rename = "setTimeoutCount", default)]
    st_total: u32,
    #[serde(rename = "setTimeoutActiveCount", default)]
    st_active: u32,
    #[serde(rename = "setTimeoutCancelCount", default)]
    st_cancel: u32,
    #[serde(rename = "setIntervalCount", default)]
    si_total: u32,
    #[serde(rename = "setIntervalActiveCount", default)]
    si_active: u32,
    #[serde(rename = "setIntervalCancelCount", default)]
    si_cancel: u32,
}

fn detected_from_cdp(a: &CdpAnimation) -> DetectedAnimation {
    let (duration_ms, delay_ms, iterations, easing) = match a.source.as_ref() {
        Some(s) => (
            s.duration,
            s.delay,
            s.iterations.unwrap_or(1.0),
            Some(s.easing.clone()),
        ),
        None => (0.0, 0.0, 1.0, None),
    };
    DetectedAnimation {
        id: a.id.clone(),
        name: cdp_name_opt(a),
        target_selector: a.css_id.clone(),
        duration_ms,
        delay_ms,
        iterations,
        easing,
    }
}
