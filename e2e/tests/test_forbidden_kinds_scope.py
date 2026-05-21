#!/usr/bin/env python3
"""Regression guard: `forbidden_kinds: ["disappearance"]` is scoped to
`expected_targets` when those are declared. A scrolled-off sibling
section that gets dropped by the visibility filter must not
false-fail the gate — the contract only "forbids" the declared
targets themselves from disappearing.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    # Part 1: forbidden_kinds is scoped to expected_targets.
    contract = {
        "url": fixture_url("scroll-removes-other-section.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": "Card rises into view after scroll; #hero is unrelated.",
            "expected_duration_ms": 600,
            "expected_kinds": ["fade", "translate"],
            "expected_targets": ["#card"],
            # The agent's intent: "the card itself must not disappear".
            # An unrelated #hero scrolling off the viewport must NOT
            # be counted as a violation.
            "forbidden_kinds": ["disappearance"],
        },
        "triggers": [
            {
                "at_t_ms": 50,
                "kind": {"kind": "scroll", "x": 0, "y": 1100},
            },
            # Remove #hero from the DOM mid-sequence to simulate a
            # genuine disappearance on a non-target element (a
            # JS-driven section unmount, e.g. routing to another view
            # or a modal teardown that wipes its container).
            {
                "at_t_ms": 200,
                "kind": {"kind": "evaluate", "js": "document.getElementById('hero').remove()"},
            },
        ],
        "sample_plan": {
            "target_times_ms": [100, 250, 400, 550, 700],
            "include_layout": True,
        },
        "thresholds": {
            "min_smoothness": 0.0,
            "max_jank_events": 99,
            "require_intent_match": True,
            "min_coverage_score": 0.0,
            "require_non_static": False,
        },
        "include_contact_sheet": False,
    }

    with McpClient() as c:
        r = c.call("motion.verify", contract)

    asm = r.get("assessment") or {}
    im = asm.get("intent_match") or {}
    disappeared = asm.get("disappeared_selectors") or []
    appeared = asm.get("appeared_selectors") or []

    # Precondition for the test: something else (the hero) did
    # actually disappear from the visible set, so the unscoped
    # behaviour would have failed.
    assert any("#hero" in s or "hero" in s for s in disappeared), (
        f"precondition: the unrelated #hero must have actually been "
        f"flagged as disappeared in the sequence; got "
        f"disappeared={disappeared}"
    )
    assert "#card" not in disappeared, (
        f"precondition: the declared #card target must NOT have "
        f"actually disappeared (it just animates in); got "
        f"disappeared={disappeared}"
    )

    # The actual fix: forbidden_kinds_seen must be empty even though
    # `detected_motion_kinds` contains `disappearance` — because no
    # expected_target was the one that disappeared.
    fks = im.get("forbidden_kinds_seen") or []
    assert "disappearance" not in fks, (
        f"`disappearance` must NOT appear in forbidden_kinds_seen "
        f"when expected_targets=[#card] and the disappearance was "
        f"actually #hero. got forbidden_kinds_seen={fks}, "
        f"disappeared_selectors={disappeared}"
    )

    # Part 2: when expected_targets is empty (free-form contract),
    # page-wide forbidden gating still works.
    contract_free = dict(contract)
    contract_free["episode_intent"] = {
        "description": "page-wide check",
        "expected_duration_ms": 600,
        "expected_kinds": [],
        "expected_targets": [],
        "forbidden_kinds": ["disappearance"],
    }
    with McpClient() as c:
        r2 = c.call("motion.verify", contract_free)
    im2 = (r2.get("assessment") or {}).get("intent_match") or {}
    fks2 = im2.get("forbidden_kinds_seen") or []
    assert "disappearance" in fks2, (
        f"page-wide forbidden_kinds still has to fire when no "
        f"expected_targets scope is set; got "
        f"forbidden_kinds_seen={fks2}"
    )

    print(
        f"OK  forbidden_kinds scoped to expected_targets "
        f"(disappeared={disappeared}, forbidden_seen={fks}), AND "
        f"page-wide mode still detects disappearance ({fks2})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
