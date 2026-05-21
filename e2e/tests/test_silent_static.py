#!/usr/bin/env python3
"""Regression guard: an undeclared, visually prominent element that
stayed still while other elements moved is surfaced as
`silent-static-prominent-element`.

`intent_match` only grades `expected_targets`, so without this hint a
hero that never animates (because the author forgot to declare it)
passes green — the agent could only catch it by eyeballing a frame.

silent-static.html: a 1100x340 hero band (~36% of a 1280x800 viewport)
that never moves, plus a small box that DOES translate on load. The
contract declares only the box. The report must still emit
`silent-static-prominent-element` pointing at the hero, so the blind
spot is closed without depending on expected_targets completeness.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    contract = {
        "url": fixture_url("silent-static.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": "Box translates on load; hero is NOT declared.",
            "expected_duration_ms": 1000,
            "expected_kinds": ["translate"],
            "expected_targets": ["div.box"],  # hero deliberately undeclared
        },
        "sample_plan": {"target_times_ms": [0, 250, 500, 750, 1000], "include_layout": True},
        "thresholds": {
            "min_smoothness": 0.0,
            "max_jank_events": 99,
            "require_intent_match": False,
            "min_coverage_score": 0.0,
            "require_non_static": True,
        },
        "include_contact_sheet": False,
    }
    with McpClient() as c:
        r = c.call("motion.verify", contract)

    hints = r.get("diagnosis_hints") or []
    ss = [h for h in hints if h.get("code") == "silent-static-prominent-element"]
    assert ss, (
        f"expected silent-static-prominent-element hint; "
        f"codes={[h.get('code') for h in hints]}"
    )
    h = ss[0]
    # It must point at the still hero, not the moving box.
    tgt = h.get("target_selector") or ""
    assert "hero" in tgt, f"hint should target the static hero, got {tgt!r}"

    obs = h.get("observed") or {}
    assert obs.get("viewport_fraction", 0.0) >= 0.06, (
        f"hero must be flagged as prominent (>=6% viewport): {obs}"
    )
    assert obs.get("moved_selectors_count", 0) >= 1, (
        f"the box moved, so moved_selectors_count must be >=1: {obs}"
    )
    assert h.get("suggested_probe"), "hint must carry a suggested_probe"

    # The box itself (declared + moving) must NOT be flagged.
    assert all("box" not in (x.get("target_selector") or "") for x in ss), (
        f"the moving/declared box must not be flagged: {[x.get('target_selector') for x in ss]}"
    )

    print(
        f"OK  hint targets {tgt!r}  viewport_fraction={obs.get('viewport_fraction'):.2f}  "
        f"moved_count={obs.get('moved_selectors_count')}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
