//! ai-motionlens-core
//!
//! Drive a Chromium browser via CDP, control the page's virtual clock,
//! capture frames with structured metadata, and accumulate evidence for an
//! AI agent's debugging loop.
//!
//! Public surface:
//! - [`Session`] : one Chromium + one [`TimeDriver`] + one [`TimelineSourceCollector`]
//! - [`TimeDriver`] : clock-only backends (primary: [`VirtualTimeDriver`])
//! - [`TimelineSourceCollector`] : observation-only backends (primary: [`CdpTimelineCollector`])
//! - typed [`Error`] / [`Result`] in [`error`]
//! - public model types in [`model`]

pub mod artifact;
pub mod driver;
pub mod error;
mod font;
pub mod model;
pub mod session;

pub use crate::driver::{
    AnimationBuffer, CdpTimelineCollector, TimeDriver, TimelineSourceCollector, VirtualTimeDriver,
};
pub use crate::error::{Error, Result};
pub use crate::model::{
    ActiveSource, AdvanceOutcome, AdvanceResult, AnimationAssessment, ArtifactId, BboxChange,
    BisectReplayKind, BisectResult, CaptureSeriesResult, ClockPolicy, ClockStatus, ContactSheet,
    ContactSheetCell, ContractTrigger, DetectedAnimation, DetectionMeta, DetectionMethod,
    DiagnosisHint, DomQueryResult, DriverKind, DurationMatch, EasingHint, EasingMatch,
    ElementProbe, EpisodeId, EpisodeIntent, EpisodeStartState, EvidenceLedger, ExternalSourceMode,
    ExternalStatePolicy, Frame, FrameId, FramePath, ImageFormat, InputMode, IntentMatchReport,
    Interval, JankEvent, LaunchOptions, LayoutAnomaly, LayoutAnomalyKind, LayoutProbeOptions,
    LayoutSnapshot, MotionAuditResult, MotionCategory, MotionContract, MotionDiff,
    MotionSuggestIntentRequest, MotionSuggestIntentResponse, MotionThresholds, MotionVerifyReport,
    MotionViewport, Observation, ObservationId, OvershootEvent, RafDetectionResult, RecipeInclude,
    ReplayCostModel, ReplayResult, RuntimeJankEvent, SamplePlan, ScrollAnimationCheckResult,
    ScrollDrivenSource, ScrollRangeMatch, ScrollSeriesResult, SectionProgressSpec,
    SeekBackStrategy, SessionCapabilities, SessionId, SourceKind, StyleChange, TargetEasing,
    TargetSpec, TimelineCategory, TimelineSources, TimerCounts, TimerDetectionResult, TriggerEvent,
    TriggerEventId, TriggerKind, UnobservedInterval, VideoExport, VideoFormat, WaitOutcome,
    WaitPolicy,
};
pub use crate::session::{run_motion_suggest_intent, run_motion_verify, Session};
