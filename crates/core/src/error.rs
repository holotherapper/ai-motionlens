//! Typed errors for the ai-motionlens-core public API.

use thiserror::Error;

/// Result alias used throughout the public API of `ai-motionlens-core`.
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("session not found: {0}")]
    SessionNotFound(String),

    #[error("episode not found: {0}")]
    EpisodeNotFound(String),

    #[error("frame not found: {0}")]
    FrameNotFound(String),

    #[error("no active episode on session {0}")]
    NoActiveEpisode(String),

    /// The driver cannot rewind (`can_seek_back == false`) and the caller
    /// requested a timestamp earlier than the current virtual clock.
    #[error("backward seek is not supported by driver `{driver}`; target {target_ms} ms < current {current_ms} ms")]
    BackwardSeekUnsupported {
        driver: &'static str,
        target_ms: f64,
        current_ms: f64,
    },

    /// `Emulation.setVirtualTimePolicy(advance, budget=N)` issued but the page
    /// kept starving virtual time with 0-delay timers/promises and
    /// `maxVirtualTimeTaskStarvationCount` was hit.
    #[error("virtual time starvation hit after {tasks_processed} tasks (budget {budget_ms} ms not exhausted)")]
    VirtualTimeStarvation {
        tasks_processed: u64,
        budget_ms: f64,
    },

    #[error("trigger failed: {reason}")]
    TriggerFailed { reason: String },

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("chromiumoxide error: {0}")]
    Browser(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

impl Error {
    pub fn browser<E: std::fmt::Display>(e: E) -> Self {
        Error::Browser(e.to_string())
    }

    pub fn trigger_failed<S: Into<String>>(reason: S) -> Self {
        Error::TriggerFailed {
            reason: reason.into(),
        }
    }

    pub fn invalid<S: Into<String>>(msg: S) -> Self {
        Error::InvalidArgument(msg.into())
    }
}
