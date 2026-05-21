#!/usr/bin/env python3
"""Regression guard: a Stats-style count-up that animates only the
element's text content (no bbox / opacity / transform movement) must
register as `text-change` in `detected_motion_kinds`, and the element
must appear in `moved_selectors`.

If `classify_transition` and `moved_selectors_between` ignored
`text_content_preview`, a count-up like `0 → 247` would be silently
dropped — `detected_motion_kinds` would never pick up a text-number
count-up, the `intent_match` for a counter would always miss, and the
agent would have no kind it could declare in `expected_kinds`.

The fixture rolls a `<div id="stat">` from `0` to `247` over 800ms via
`requestAnimationFrame` using the rAF `t` argument (virtual-clock
friendly per SKILL.md "Animating under the virtual clock").
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    contract = {
        "url": fixture_url("text-count-up.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": "Stats counter rolls 0 → 247 over 800ms.",
            "expected_duration_ms": 800,
            "expected_kinds": ["text-change"],
            "expected_targets": ["#stat"],
        },
        "sample_plan": {"target_times_ms": [0, 200, 400, 600, 800], "include_layout": True},
        "thresholds": {
            "min_smoothness": 0.0,
            "max_jank_events": 99,
            "require_intent_match": True,
            "min_coverage_score": 0.0,
            # A text-only count-up does change pixels (digit glyphs),
            # so non-static is satisfied; keep it on as a smoke check.
            "require_non_static": True,
        },
        "include_contact_sheet": False,
    }

    with McpClient() as c:
        r = c.call("motion.verify", contract)

    assessment = r.get("assessment") or {}
    detected = assessment.get("detected_motion_kinds") or []
    moved = assessment.get("moved_selectors") or []
    im = assessment.get("intent_match") or {}

    assert "text-change" in detected, (
        f"a text count-up must register as `text-change` in "
        f"detected_motion_kinds; got detected={detected}"
    )
    assert "#stat" in moved, (
        f"the counter selector must be in moved_selectors so "
        f"expected_targets can grade it; got moved={moved}"
    )
    assert im.get("passes") is True, (
        f"intent_match.passes must be true for "
        f"`expected_kinds: [text-change]` on a text count-up; got "
        f"intent_match={im}, failed_gates={r.get('failed_gates')}"
    )
    print(
        f"OK  text count-up registered as text-change; detected={detected} "
        f"moved={moved} intent_match.passes=true"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
