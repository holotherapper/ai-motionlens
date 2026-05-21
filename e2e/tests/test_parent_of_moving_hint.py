#!/usr/bin/env python3
"""Regression guard: when an agent declares the parent in
`expected_targets` while the animation runs on its children (the
canonical text-stagger pattern), `motion.verify` must surface an
`intent-target-parent-of-moving-element` hint naming the moving
descendants, NOT a `raf-source-stalled` candidate-cause list (the page
IS animating, on the children — there's no rAF stall).
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    contract = {
        "url": fixture_url("parent-static-children-move.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": "Hero lede animates in (parent declared, children move).",
            "expected_duration_ms": 900,
            "expected_kinds": ["translate"],
            # The trap: declare the parent. The fixture's actual motion
            # is on `.word`, but a real agent often grabs the visible
            # outer element first.
            "expected_targets": ["#hero-lede"],
        },
        "sample_plan": {
            "target_times_ms": [0, 150, 300, 450, 600, 900],
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

    parent_hints = [h for h in hints if h.get("code") == "intent-target-parent-of-moving-element"]
    assert parent_hints, (
        f"`intent-target-parent-of-moving-element` must fire when the "
        f"declared parent is static and its children move; got codes={codes}"
    )
    h = parent_hints[0]
    assert h.get("target_selector") == "#hero-lede", (
        f"target_selector must point at the declared parent; got {h}"
    )
    descendants = (h.get("observed") or {}).get("moving_descendant_selectors") or []
    assert any("word" in s for s in descendants), (
        f"moving_descendant_selectors must name the children that "
        f"actually moved; got {descendants}"
    )
    # The hint suppresses noisier per-selector dives for the same
    # target. `raf-source-stalled` may still fire as a global hint
    # elsewhere (it's keyed off the page-level rAF / target list, not
    # this specific selector), but the per-selector dive list for
    # `#hero-lede` must not redundantly emit selector-not-found.
    snf_for_parent = [
        h for h in hints
        if h.get("code") == "selector-not-found" and h.get("target_selector") == "#hero-lede"
    ]
    assert not snf_for_parent, (
        f"selector-not-found must be suppressed for the declared "
        f"parent when the parent-of-moving hint already explained the "
        f"situation; got {snf_for_parent}"
    )
    print(
        f"OK  intent-target-parent-of-moving-element fired with "
        f"descendants={descendants}; selector-not-found suppressed "
        f"for #hero-lede"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
