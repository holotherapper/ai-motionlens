# ai-motionlens

[![CI](https://github.com/holotherapper/ai-motionlens/actions/workflows/ci.yml/badge.svg)](https://github.com/holotherapper/ai-motionlens/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange.svg)](https://www.rust-lang.org)

> **Playwright captures the current frame. ai-motionlens captures the time axis.**

An MCP tool suite that lets AI coding agents **autonomously verify web animations**. It freezes the browser's virtual clock, advances it by exact milliseconds, captures deterministic frames, and grades the result against a contract — so the agent can design, implement, observe, evaluate, and improve animations without human intervention.

## Features

- **33 MCP tools** — session management, virtual-clock control, triggers, frame capture, layout probes, motion assessment, video export, library introspection (GSAP / Lenis / Lottie)
- **Animation Evidence Gate** (`motion.verify`) — one MCP call that runs the full observe → score → report loop and returns a structured pass/fail verdict
- **Diagnosis hints** — root-cause candidates (`selector-not-found`, `hidden-by-zero-opacity`, `jank-spike`, …) with a `suggested_probe` pointing to the next tool call
- **Per-target easing fit** — RMS-fit against 7 easing templates, stagger uniformity, overshoot detection, runtime jank (Chrome LoAF)
- **Deterministic replay** — pin `Math.random`, storage, timezone, locale, and user-agent for reproducible captures
- **`animation-qa` sub-agent** — a tools-restricted sub-agent that physically cannot call Playwright

## Install

**Prerequisites:** Google Chrome or Chromium (`CHROME_PATH` to override)

```sh
brew install holotherapper/tap/ai-motionlens
```

Or from source (requires Rust 1.80+):

```sh
cargo install --git https://github.com/holotherapper/ai-motionlens ai-motionlens ai-motionlens-mcp
```

## Setup for Claude Code

### Plugin (recommended)

Registers the MCP server, skill, and sub-agent in one step:

```
/plugin marketplace add holotherapper/ai-motionlens
/plugin install ai-motionlens@ai-motionlens
```

### Manual

```sh
claude mcp add -s user ai-motionlens -- ai-motionlens-mcp
```

## How the AI uses it

The agent calls `motion.verify` with a contract describing the expected animation:

```jsonc
// One MCP call → full verification
{
  "url": "http://localhost:5173/",
  "viewport": { "width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": true },
  "episode_intent": {
    "description": "Hero parallax fades + translates between scroll 0 and 1000 px.",
    "expected_duration_ms": 800,
    "expected_kinds": ["fade", "translate"],
    "expected_targets": ["#hero"]
  },
  "sample_plan": {
    "target_times_ms": [0, 60, 150, 300, 500, 800],
    "include_layout": true
  },
  "thresholds": {
    "min_smoothness": 0.6,
    "max_jank_events": 0,
    "require_intent_match": true
  },
  "include_contact_sheet": true
}
```

The response is a `MotionVerifyReport` with `passes`, `verdict`, `diagnosis_hints`, per-target easing fit, and a contact sheet. If the gate fails, `diagnosis_hints[].suggested_probe` tells the agent exactly which tool call to run next.

> [!TIP]
> The agent can call `motion.suggest_intent` first to probe the page and auto-draft the contract — no human guesswork needed.

For delegation, the parent agent can hand off to the `animation-qa` sub-agent:

```
Agent({ subagent_type: "animation-qa",
        prompt: "Verify http://localhost:5173/ — hero should fade + translate over 800ms." })
```

## How it works

1. **`session.launch`** — spawns headless Chrome, freezes the virtual clock at navigation commit
2. **`trigger.*`** — dispatches click / hover / scroll / evaluate at the current frozen time
3. **`clock.advance`** — advances the virtual clock by exact milliseconds; the page renders deterministically
4. **`frame.capture`** — screenshot + layout snapshot (bbox, opacity, transform, running animations)
5. **`motion.assess`** — derives smoothness, jank, per-target easing, stagger, overshoot
6. **`motion.verify`** — orchestrates 1–5 end-to-end, grades against thresholds, writes `motionlens-report.json`

The key insight: **the AI controls time, not the other way around**. No `setTimeout` guessing, no real-time racing — every frame is captured at an exact, reproducible virtual timestamp.

## Architecture

```
ai-motionlens/
├── crates/
│   ├── core/       # Session, VirtualTimeDriver, motion.assess, frame capture
│   ├── mcp/        # MCP server (33 tools, rmcp 1.7)
│   └── cli/        # CLI for CI: verify, gate-check, shot, series, timeline, bisect
├── plugin/         # Claude Code plugin (MCP + skill + sub-agent)
├── skill/          # SKILL.md + reference recipes
├── agents/         # animation-qa sub-agent definition
└── e2e/            # 57 Python E2E tests + 34 HTML fixtures
```

<details>
<summary><strong>CLI reference (for CI integration)</strong></summary>

The CLI is primarily for CI pipelines, not direct human use:

```sh
# Run the evidence gate in CI
ai-motionlens verify --config motionlens.config.json --out motionlens-report.json
# exit 0 = pass, exit 1 = fail

# Validate an existing report
ai-motionlens gate-check --report motionlens-report.json

# Low-level frame capture (debugging)
ai-motionlens shot --url <url> --at-ms 150 --out frame.png
ai-motionlens series --url <url> --target-times-ms 0,100,300,500,1000 --out-prefix frames
ai-motionlens timeline --url <url>
ai-motionlens bisect --url <url> --t0-ms 100 --t1-ms 300 --out mid.png
```

</details>
