#!/usr/bin/env python3
"""Regression guard for the "never hang us" invariant.

A CSS `@keyframes` animation armed by a trigger, then sampled at virtual
t=0 with no intervening `clock.advance`, may produce no compositor frame
— `Page.captureScreenshot` would otherwise block forever. `screenshot()`
bounds that wait with an 8s timeout.

Whether a compositor frame is committed under the paused virtual clock at
that exact instant is *non-deterministic* (the pause-mode
`captureScreenshot` / compositor-commit timing has no per-frame barrier
in current headless Chrome — a known platform limitation). So
`motion.verify` here decides one of two *honest* ways, and the guard
must accept either:

  (a) no frame committed  -> the bounded-capture path raises the typed
      timeout error (no fabricated frame), or
  (b) a frame did commit  -> a real report backed by on-disk artifacts.

What this guard actually protects (these are the regressions that matter):

  1. it stays *bounded* — never the original 110s+ indefinite hang, and
  2. it never *lies* — an error is the typed bounded-capture error (not a
     silently fabricated frame), and a report is backed by real frames on
     disk (not a fabricated success).

`passes` is intentionally NOT asserted: when the compositor does commit,
the trigger-armed animation is observed legitimately and may pass — that
is a correct observation, not a fake one.
"""
from __future__ import annotations

import os
import pathlib
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url

# The hang was 110s+. The bounded path (Chrome launch + nav + trigger +
# one 8s-capped capture attempt) returns well under this. Generous, but
# decisively below an indefinite hang.
MAX_ELAPSED_S = 75.0


def main() -> int:
    contract = {
        "url": fixture_url("jank-triggered.html"),
        "viewport": {
            "width": 1280,
            "height": 800,
            "device_scale_factor": 1.0,
            "headless": True,
        },
        "episode_intent": {
            "description": "Box translates over 1000ms after the trigger arms it.",
            "expected_duration_ms": 1000,
            "expected_kinds": ["translate"],
            "expected_targets": ["div.box"],
        },
        "triggers": [
            {
                "at_t_ms": 0,
                "kind": {
                    "kind": "evaluate",
                    "js": "window.startJank()",
                    "frame_path": [],
                },
                "wait_policy": "none",
            }
        ],
        # Sample starts at t=0 with the trigger also at t=0 -> capture
        # happens right after the @keyframes is armed, with no advance.
        "sample_plan": {
            "target_times_ms": [0, 200, 500, 1000],
            "include_layout": True,
        },
        "thresholds": {
            "min_smoothness": 0.6,
            "max_jank_events": 0,
            "require_intent_match": False,
            "min_coverage_score": 0.0,
            "require_non_static": True,
        },
        "include_contact_sheet": False,
    }

    with McpClient() as c:
        t0 = time.monotonic()
        outcome = ""  # "error" | "report"
        msg = ""
        report = None
        try:
            report = c.call("motion.verify", contract)
            outcome = "report"
        except RuntimeError as e:
            outcome = "error"
            msg = str(e)
        elapsed = time.monotonic() - t0

    # (1) Core invariant: bounded — never the original 110s+ hang.
    assert elapsed < MAX_ELAPSED_S, (
        f"capture hang guard BROKEN: motion.verify took {elapsed:.1f}s "
        f"(>= {MAX_ELAPSED_S}s) — the indefinite hang has regressed"
    )

    # (2) Honest outcome. Compositor-commit timing is non-deterministic
    #     under the paused clock (DESIGN §5.1) so either path is valid as
    #     long as it does not fabricate.
    if outcome == "error":
        assert "screenshot timed out" in msg or "compositor frame" in msg, (
            f"error must be the bounded-capture typed error (no fabricated "
            f"frame), got: {msg}"
        )
        detail = f"error~={msg[:80]!r}"
    else:
        frames = report.get("frames") or []
        assert frames, (
            f"report returned with no frames (fabricated success?): "
            f"{report.get('verdict_human')}"
        )
        for f in frames:
            p = f.get("artifact_local_path")
            assert p and os.path.exists(p) and os.path.getsize(p) > 0, (
                f"frame t={f.get('t_ms')} has no real on-disk artifact "
                f"(fabricated?): {p!r}"
            )
        detail = f"frames={len(frames)} passes={report.get('passes')}"

    print(f"OK  elapsed={elapsed:.1f}s  outcome={outcome}  {detail}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
