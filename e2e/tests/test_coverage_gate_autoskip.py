#!/usr/bin/env python3
"""Regression guard: on a page driven entirely by rAF / GSAP / Framer
Motion / Lottie (no CSS Animation / Transition / WAAPI source), the
`coverage` gate is auto-skipped. The agent doesn't have to declare
`min_coverage_score: 0.0` for every JS-driven page — `coverage_score`
is structurally unmeasurable when no CDP-known source exists, so
gating on it is meaningless.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    # Agent contract written with skill's standard default
    # `min_coverage_score: 0.6` — the lack of CSS sources must not fire
    # a `coverage` advisory gate.
    contract = {
        "url": fixture_url("raf-only-counter.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": "Counter rolls 0 → 100 over 800ms via rAF.",
            "expected_duration_ms": 800,
            "expected_kinds": ["text-change"],
            "expected_targets": ["#stat"],
        },
        "sample_plan": {"target_times_ms": [0, 200, 400, 600, 800], "include_layout": True},
        "thresholds": {
            "min_smoothness": 0.0,
            "max_jank_events": 99,
            "require_intent_match": True,
            # Standard default — must NOT cause a fail when the page
            # has no CDP-known sources.
            "min_coverage_score": 0.6,
            "require_non_static": True,
        },
        "include_contact_sheet": False,
    }

    with McpClient() as c:
        r = c.call("motion.verify", contract)

    fg = r.get("failed_gates") or []
    coverage_gates = [g for g in fg if g.get("gate") == "coverage"]
    assert not coverage_gates, (
        f"coverage gate must be auto-skipped on a rAF-only page; "
        f"got {coverage_gates}"
    )

    em = r.get("evidence_missing") or []
    assert any("coverage" in e.lower() and ("rAF" in e or "raf" in e.lower()) for e in em), (
        f"evidence_missing must explain the coverage skip (rAF/GSAP "
        f"driven); got {em}"
    )

    # And the headline passes when intent-match resolves.
    assert r.get("passes") is True, (
        f"the whole report must pass — intent_match green, "
        f"non-static green, no spurious coverage failure; got "
        f"passes={r.get('passes')}, failed_gates={fg}"
    )
    print(
        f"OK  rAF-only page: coverage gate auto-skipped, "
        f"evidence_missing carries the explanation, headline "
        f"passes=True"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
