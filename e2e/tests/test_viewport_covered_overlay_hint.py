#!/usr/bin/env python3
"""Regression guard: when a fullscreen overlay (loader / splash /
modal backdrop) covers the viewport across every sampled frame while
intent_match isn't fully green, `motion.verify` must surface a
`viewport-covered-by-overlay` hint with an evaluate-trigger fragment
in `suggested_probe`. Without this, agents have historically given
up on motion.verify and fallen back to `Playwright.browser_take_
screenshot` to confirm visually — exactly the anti-pattern the skill
exists to prevent.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    contract = {
        "url": fixture_url("overlay-hides-content.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": "The chip rises 20px over 600ms, behind a fullscreen loader.",
            "expected_duration_ms": 600,
            "expected_kinds": ["translate"],
            # The agent would naturally declare the behind-overlay
            # target; the verify pass can't actually see it move via
            # bbox sampling because the overlay blocks the per-frame
            # screenshot delta.
            "expected_targets": ["#chip"],
        },
        "sample_plan": {
            "target_times_ms": [0, 150, 300, 450, 600],
            "include_layout": True,
        },
        "thresholds": {
            "min_smoothness": 0.0,
            "max_jank_events": 99,
            "require_intent_match": False,
            "min_coverage_score": 0.0,
            "require_non_static": False,
        },
        "include_contact_sheet": False,
    }

    with McpClient() as c:
        r = c.call("motion.verify", contract)

    hints = r.get("diagnosis_hints") or []
    codes = [h.get("code") for h in hints]

    overlay_hints = [h for h in hints if h.get("code") == "viewport-covered-by-overlay"]
    assert overlay_hints, (
        f"`viewport-covered-by-overlay` must fire when a fullscreen "
        f"loader covers every captured frame and intent_match isn't "
        f"green; got codes={codes}"
    )
    h = overlay_hints[0]
    assert h.get("target_selector") == "#loader", (
        f"target_selector must point at the covering element; got "
        f"{h.get('target_selector')!r}"
    )
    obs = h.get("observed") or {}
    assert obs.get("covered_in_every_frame") is True, (
        f"observed.covered_in_every_frame must be true; got {obs}"
    )
    cov = obs.get("coverage_fraction") or 0
    assert cov >= 0.85, (
        f"observed.coverage_fraction must reflect the actual coverage "
        f"(>=0.85 by construction); got {cov}"
    )
    sp = h.get("suggested_probe") or ""
    assert "evaluate" in sp and "display = 'none'" in sp, (
        f"suggested_probe must include a ready-to-paste evaluate "
        f"trigger that hides the overlay; got {sp!r}"
    )
    print(
        f"OK  viewport-covered-by-overlay fired for #loader "
        f"(coverage={cov:.2f}); suggested_probe carries an evaluate "
        f"trigger fragment"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
