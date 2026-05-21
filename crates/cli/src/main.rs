//! ai-motionlens CLI
//!
//! Direct command-line access to ai-motionlens-core without MCP. Intended for
//! shell scripts, CI jobs, and ad-hoc debugging.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tokio::fs;

use ai_motionlens_core::{
    run_motion_verify, ImageFormat, InputMode, Interval, LaunchOptions, MotionContract, Session,
    TargetSpec, TriggerKind, WaitPolicy,
};

#[derive(Parser, Debug)]
#[command(
    name = "ai-motionlens",
    version,
    about = "AI-driven web animation debugging - freeze the page virtual clock, advance to absolute virtual milliseconds, capture frames with structured evidence."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Capture a single frame at an absolute virtual timestamp (ms from episode start).
    Shot {
        #[arg(long)]
        url: String,
        /// Absolute virtual milliseconds at which to capture.
        #[arg(long, default_value_t = 0.0)]
        at_ms: f64,
        #[arg(long, default_value = "shot.png")]
        out: PathBuf,
        #[arg(long, default_value_t = 1280)]
        viewport_width: u32,
        #[arg(long, default_value_t = 800)]
        viewport_height: u32,
        /// Run with a visible browser window. Default is headless.
        #[arg(long)]
        headed: bool,
        /// CSS selector to click before capture (records into the active episode).
        #[arg(long)]
        click: Option<String>,
    },

    /// Capture a series of frames at absolute virtual-time targets (ascending).
    Series {
        #[arg(long)]
        url: String,
        /// Comma-separated absolute virtual milliseconds, e.g. "0,100,300,1000".
        #[arg(long, value_delimiter = ',')]
        target_times_ms: Vec<f64>,
        /// Output file name prefix; each frame is written as `<prefix>_<t_ms>.png`.
        #[arg(long, default_value = "series")]
        out_prefix: String,
        #[arg(long, default_value_t = 1280)]
        viewport_width: u32,
        #[arg(long, default_value_t = 800)]
        viewport_height: u32,
        #[arg(long)]
        headed: bool,
        /// CSS selector to click before the capture series begins.
        #[arg(long)]
        click: Option<String>,
    },

    /// Detect what time-based animation sources exist on the page (JSON to stdout).
    Timeline {
        #[arg(long)]
        url: String,
        #[arg(long, default_value_t = 1280)]
        viewport_width: u32,
        #[arg(long, default_value_t = 800)]
        viewport_height: u32,
        #[arg(long)]
        headed: bool,
    },

    /// Run an Animation Evidence Gate from a `motionlens.config.json`
    /// contract. Drives the entire verify loop (launch → episode → triggers
    /// → capture_series → assess → coverage) and writes the report. Exit
    /// code is 0 on `passes: true`, 1 otherwise — CI-friendly.
    Verify {
        /// Path to the contract file (JSON). The schema is `MotionContract`.
        #[arg(long, default_value = "motionlens.config.json")]
        config: PathBuf,
        /// Where to write the resulting report. Defaults to
        /// `motionlens-report.json` in the current working directory; the
        /// report is ALSO written to the session's artifact directory.
        #[arg(long, default_value = "motionlens-report.json")]
        out: PathBuf,
        /// Print the full report JSON to stdout in addition to writing it.
        #[arg(long)]
        print: bool,
    },

    /// Validate an existing `motionlens-report.json` against the contract.
    /// Exit 0 = gate passes, exit 1 = gate fails (stderr carries the reason).
    /// Used by CI to decide whether the evidence gate is satisfied — checks
    /// the report exists, parses, has `passes: true`, matches the contract
    /// URL/viewport when a config is available, and is not staler than the
    /// freshest source file in cwd.
    GateCheck {
        /// Path to the report to validate.
        #[arg(long, default_value = "motionlens-report.json")]
        report: PathBuf,
        /// Optional path to the contract used to produce the report. When
        /// present, URL and viewport are cross-checked.
        #[arg(long, default_value = "motionlens.config.json")]
        config: PathBuf,
        /// Skip the source-mtime freshness check. Useful for one-shot
        /// integration tests; leave disabled in normal operation.
        #[arg(long)]
        skip_freshness: bool,
    },

    /// Capture the midpoint of an unobserved interval. If the midpoint is in
    /// the past relative to the live clock, performs a scratch-page replay.
    Bisect {
        #[arg(long)]
        url: String,
        #[arg(long)]
        t0_ms: f64,
        #[arg(long)]
        t1_ms: f64,
        /// Optional click selector to record into the episode before bisecting.
        #[arg(long)]
        click: Option<String>,
        /// Drive the live clock to this timestamp before invoking bisect, to
        /// force the scratch-replay path.
        #[arg(long, default_value_t = 0.0)]
        advance_to_ms: f64,
        #[arg(long, default_value = "bisect.png")]
        out: PathBuf,
        #[arg(long, default_value_t = 1280)]
        viewport_width: u32,
        #[arg(long, default_value_t = 800)]
        viewport_height: u32,
        #[arg(long)]
        headed: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ai_motionlens=info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Shot {
            url,
            at_ms,
            out,
            viewport_width,
            viewport_height,
            headed,
            click,
        } => {
            shot(ShotOpts {
                url,
                at_ms,
                out,
                viewport_width,
                viewport_height,
                headed,
                click,
            })
            .await
        }
        Command::Series {
            url,
            target_times_ms,
            out_prefix,
            viewport_width,
            viewport_height,
            headed,
            click,
        } => {
            series(SeriesOpts {
                url,
                target_times_ms,
                out_prefix,
                viewport_width,
                viewport_height,
                headed,
                click,
            })
            .await
        }
        Command::Timeline {
            url,
            viewport_width,
            viewport_height,
            headed,
        } => timeline(url, viewport_width, viewport_height, headed).await,
        Command::Verify { config, out, print } => verify(config, out, print).await,
        Command::GateCheck {
            report,
            config,
            skip_freshness,
        } => gate_check(report, config, skip_freshness).await,
        Command::Bisect {
            url,
            t0_ms,
            t1_ms,
            click,
            advance_to_ms,
            out,
            viewport_width,
            viewport_height,
            headed,
        } => {
            bisect(BisectOpts {
                url,
                t0_ms,
                t1_ms,
                click,
                advance_to_ms,
                out,
                viewport_width,
                viewport_height,
                headed,
            })
            .await
        }
    }
}

fn launch_options(
    url: String,
    viewport_width: u32,
    viewport_height: u32,
    headed: bool,
) -> LaunchOptions {
    LaunchOptions {
        url,
        viewport_width,
        viewport_height,
        headless: !headed,
        device_scale_factor: 1.0,
        external_state: None,
        external_state_seed: 0,
    }
}

/// Click `selector` in the active episode, if one was requested.
async fn click_if_requested(session: &mut Session, click: Option<String>) -> Result<()> {
    let Some(selector) = click else {
        return Ok(());
    };
    session
        .trigger(
            TriggerKind::Click {
                target: TargetSpec {
                    selector,
                    frame_path: Vec::new(),
                    resolved_coordinates: None,
                },
            },
            WaitPolicy::None,
            InputMode::Js,
        )
        .await
        .context("trigger.click")?;
    Ok(())
}

struct ShotOpts {
    url: String,
    at_ms: f64,
    out: PathBuf,
    viewport_width: u32,
    viewport_height: u32,
    headed: bool,
    click: Option<String>,
}

async fn shot(opts: ShotOpts) -> Result<()> {
    let ShotOpts {
        url,
        at_ms,
        out,
        viewport_width,
        viewport_height,
        headed,
        click,
    } = opts;
    if at_ms < 0.0 {
        anyhow::bail!("--at-ms must be >= 0");
    }
    let launch = launch_options(url, viewport_width, viewport_height, headed);
    let mut session = Session::launch(launch).await.context("Session::launch")?;
    session.start_episode().context("start_episode")?;
    click_if_requested(&mut session, click).await?;

    if at_ms > 0.0 {
        session
            .clock_advance(at_ms, None)
            .await
            .context("clock_advance")?;
    }

    let frame = session
        .capture_frame(ImageFormat::Png, false, None, false)
        .await
        .context("capture_frame")?;

    if let Some(local) = session.artifact_local_path(&frame.artifact_uri) {
        fs::copy(&local, &out)
            .await
            .with_context(|| format!("copy {} -> {}", local.display(), out.display()))?;
        println!(
            "wrote {} ({}x{}, t={:.1}ms)",
            out.display(),
            frame.width,
            frame.height,
            frame.t_ms
        );
    } else {
        anyhow::bail!(
            "artifact path could not be resolved from URI {}",
            frame.artifact_uri
        );
    }

    session.close().await.context("session.close")?;
    Ok(())
}

struct SeriesOpts {
    url: String,
    target_times_ms: Vec<f64>,
    out_prefix: String,
    viewport_width: u32,
    viewport_height: u32,
    headed: bool,
    click: Option<String>,
}

async fn series(opts: SeriesOpts) -> Result<()> {
    let SeriesOpts {
        url,
        target_times_ms,
        out_prefix,
        viewport_width,
        viewport_height,
        headed,
        click,
    } = opts;
    if target_times_ms.is_empty() {
        anyhow::bail!("--target-times-ms must contain at least one value");
    }
    let launch = launch_options(url, viewport_width, viewport_height, headed);
    let mut session = Session::launch(launch).await.context("Session::launch")?;
    session.start_episode().context("start_episode")?;
    click_if_requested(&mut session, click).await?;

    let result = session
        .capture_series(&target_times_ms, ImageFormat::Png, false, None, false)
        .await
        .context("capture_series")?;

    for frame in &result.frames {
        let out = PathBuf::from(format!("{}_{:.0}.png", out_prefix, frame.t_ms));
        if let Some(local) = session.artifact_local_path(&frame.artifact_uri) {
            fs::copy(&local, &out)
                .await
                .with_context(|| format!("copy {} -> {}", local.display(), out.display()))?;
            println!(
                "wrote {} ({}x{}, t={:.1}ms)",
                out.display(),
                frame.width,
                frame.height,
                frame.t_ms
            );
        }
    }
    println!(
        "captured {} frames, {} unobserved intervals",
        result.frames.len(),
        result.unobserved_intervals.len()
    );

    session.close().await.context("session.close")?;
    Ok(())
}

struct BisectOpts {
    url: String,
    t0_ms: f64,
    t1_ms: f64,
    click: Option<String>,
    advance_to_ms: f64,
    out: PathBuf,
    viewport_width: u32,
    viewport_height: u32,
    headed: bool,
}

async fn bisect(opts: BisectOpts) -> Result<()> {
    let BisectOpts {
        url,
        t0_ms,
        t1_ms,
        click,
        advance_to_ms,
        out,
        viewport_width,
        viewport_height,
        headed,
    } = opts;
    let launch = launch_options(url, viewport_width, viewport_height, headed);
    let mut session = Session::launch(launch).await.context("Session::launch")?;
    let (episode_id, _) = session.start_episode().context("start_episode")?;
    click_if_requested(&mut session, click).await?;

    if advance_to_ms > 0.0 {
        session
            .clock_advance(advance_to_ms, None)
            .await
            .context("clock_advance")?;
    }

    let result = session
        .bisect(&episode_id, Interval::new(t0_ms, t1_ms))
        .await
        .context("bisect")?;

    if let Some(local) = session.artifact_local_path(&result.frame.artifact_uri) {
        tokio::fs::copy(&local, &out)
            .await
            .with_context(|| format!("copy {} -> {}", local.display(), out.display()))?;
        println!(
            "wrote {} at t={:.1}ms (replay={:?}, cost={:.1}ms)",
            out.display(),
            result.frame.t_ms,
            result.replay,
            result.replay_cost_ms
        );
    }

    session.close().await.context("session.close")?;
    Ok(())
}

async fn timeline(
    url: String,
    viewport_width: u32,
    viewport_height: u32,
    headed: bool,
) -> Result<()> {
    let opts = launch_options(url, viewport_width, viewport_height, headed);
    let mut session = Session::launch(opts).await.context("Session::launch")?;
    let sources = session
        .timeline_sources()
        .await
        .context("timeline_sources")?;
    let json = serde_json::to_string_pretty(&sources)?;
    println!("{json}");
    session.close().await.context("session.close")?;
    Ok(())
}

/// Walk `root` (skipping common build/output directories) and return the
/// mtime of the newest front-end source file, with its path.
async fn newest_source_mtime(root: &Path) -> (SystemTime, Option<PathBuf>) {
    const SKIP: [&str; 5] = ["node_modules", "target", "dist", "build", ".git"];
    const SOURCE_EXT: [&str; 9] = [
        "ts", "tsx", "js", "jsx", "css", "scss", "html", "svelte", "vue",
    ];

    let mut newest = SystemTime::UNIX_EPOCH;
    let mut newest_path: Option<PathBuf> = None;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name();
            if SKIP.iter().any(|s| *s == name.to_string_lossy()) {
                continue;
            }
            let Ok(ty) = entry.file_type().await else {
                continue;
            };
            let path = entry.path();
            if ty.is_dir() {
                stack.push(path);
                continue;
            }
            if !ty.is_file() {
                continue;
            }
            let is_source = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| SOURCE_EXT.contains(&e));
            if !is_source {
                continue;
            }
            let Ok(meta) = entry.metadata().await else {
                continue;
            };
            let Ok(m) = meta.modified() else {
                continue;
            };
            if m > newest {
                newest = m;
                newest_path = Some(path);
            }
        }
    }
    (newest, newest_path)
}

async fn gate_check(
    report_path: PathBuf,
    config_path: PathBuf,
    skip_freshness: bool,
) -> Result<()> {
    let mut violations: Vec<String> = Vec::new();

    // (a) report exists and parses
    let raw = match fs::read_to_string(&report_path).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "gate-check FAIL — report missing: {}",
                report_path.display()
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!(
                "gate-check FAIL — cannot read report ({}): {}",
                report_path.display(),
                e
            );
            std::process::exit(1);
        }
    };
    let report: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "gate-check FAIL — report is not valid JSON ({}): {}",
                report_path.display(),
                e
            );
            std::process::exit(1);
        }
    };

    // (b)(c) passes / verdict
    let passes = report
        .get("passes")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let verdict = report.get("verdict").and_then(|v| v.as_str()).unwrap_or("");
    if !passes {
        violations.push(format!("report.passes is not true (verdict={})", verdict));
    }
    if !matches!(verdict, "pass" | "needs-attention") {
        violations.push(format!(
            "report.verdict not in {{pass, needs-attention}}: {}",
            verdict
        ));
    }

    // (d) optional contract cross-check
    if config_path.exists() {
        let cfg_raw = fs::read_to_string(&config_path)
            .await
            .with_context(|| format!("read config: {}", config_path.display()))?;
        let cfg: serde_json::Value = serde_json::from_str(&cfg_raw)
            .with_context(|| format!("parse config: {}", config_path.display()))?;
        let cfg_url = cfg.get("url").and_then(|v| v.as_str()).unwrap_or("");
        // The report mirrors the contract URL into `contract_url`. A mismatch
        // means the session opened a different URL than the gate is being
        // asked to certify.
        if let Some(report_contract_url) = report.get("contract_url").and_then(|v| v.as_str()) {
            if !cfg_url.is_empty() && cfg_url != report_contract_url {
                violations.push(format!(
                    "contract URL mismatch (config={}, report={})",
                    cfg_url, report_contract_url
                ));
            }
        }
        // The report mirrors the contract's viewport into `contract_viewport`,
        // not `viewport` — use the canonical key on both sides.
        if let (Some(cfg_vw), Some(report_vw)) =
            (cfg.get("viewport"), report.get("contract_viewport"))
        {
            if cfg_vw != report_vw {
                violations.push("contract viewport differs from report viewport".into());
            }
        }
    }

    // (e) staleness: the report must be at least as new as every source file
    // under cwd (excluding common build/output paths).
    if !skip_freshness {
        let report_mtime = fs::metadata(&report_path)
            .await
            .map(|m| m.modified().unwrap_or(SystemTime::UNIX_EPOCH))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let cwd = std::env::current_dir().context("cwd")?;
        let (newest_src, newest_src_path) = newest_source_mtime(&cwd).await;
        if newest_src > report_mtime {
            let p = newest_src_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(unknown)".into());
            violations.push(format!(
                "report is stale — {} was modified after motionlens-report.json was written",
                p
            ));
        }
    }

    if violations.is_empty() {
        eprintln!(
            "gate-check OK — report={} verdict={} passes={}",
            report_path.display(),
            verdict,
            passes
        );
        Ok(())
    } else {
        eprintln!("gate-check FAIL:");
        for v in &violations {
            eprintln!("  - {}", v);
        }
        std::process::exit(1);
    }
}

async fn verify(config: PathBuf, out: PathBuf, print: bool) -> Result<()> {
    let raw = fs::read_to_string(&config)
        .await
        .with_context(|| format!("read contract: {}", config.display()))?;
    let contract: MotionContract = serde_json::from_str(&raw)
        .with_context(|| format!("parse contract: {}", config.display()))?;
    let report = run_motion_verify(contract).await.context("motion.verify")?;
    let report_json = serde_json::to_string_pretty(&report)?;
    fs::write(&out, &report_json)
        .await
        .with_context(|| format!("write report: {}", out.display()))?;
    if print {
        println!("{}", report_json);
    }
    eprintln!(
        "motion.verify: passes={} confidence={:.2} coverage={:.2} jank={} smoothness={:.2} verdict={} report={}",
        report.passes,
        report.confidence,
        report.coverage_score,
        report.assessment.jank_events.len(),
        report.assessment.smoothness,
        report.assessment.smoothness_verdict,
        report.artifact_local_path,
    );
    if !report.passes {
        // Non-zero exit so CI can fail the build on evidence-gate violation.
        std::process::exit(1);
    }
    Ok(())
}
