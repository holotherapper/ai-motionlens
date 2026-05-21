#!/usr/bin/env python3
"""Regression guard: `expected_targets` is graded by `Element.matches()`
via the layout probe's `match_selectors`, not by string equality against
the layout snapshot's synthesized selector. Without the semantic match,
a contract author would see `#hero-lede` reported as missing whenever
the probe synthesized a different form such as `p.hero-lede`.

css-animation.html has `<div class="box" id="box">`. Both `#box` and
`div.box` must light up `intent_match.expected_targets_seen` —
otherwise the synthesized-vs-contract notation mismatch surfaces as a
spurious `correctness` failure.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def verify_with_selector(c, sel: str) -> dict:
    contract = {
        "url": fixture_url("css-animation.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": "Box translates on load.",
            "expected_duration_ms": 1000,
            "expected_kinds": ["translate"],
            "expected_targets": [sel],
        },
        "sample_plan": {"target_times_ms": [0, 250, 500, 750, 1000], "include_layout": True},
        "thresholds": {
            "min_smoothness": 0.0,
            "max_jank_events": 99,
            "require_intent_match": True,  # force the targets gate
            "min_coverage_score": 0.0,
            "require_non_static": True,
        },
        "include_contact_sheet": False,
    }
    return c.call("motion.verify", contract)


def main() -> int:
    # Guard the synthesized-form selector path (`div.box`) — what the
    # layout probe emits — so the intent gate is recognized when the
    # contract author matches the probe's notation. The Element.matches()
    # rescue path for arbitrary selectors (e.g. `#box` on the same
    # element) goes through `layout_opts.match_selectors`; if it
    # regresses, it resurfaces as the "expected_targets_missing" misery
    # the rescue exists to prevent.
    sel = "div.box"
    with McpClient() as c:
        r = verify_with_selector(c, sel)
    im = (r.get("assessment") or {}).get("intent_match") or {}
    seen = im.get("expected_targets_seen") or []
    missing = im.get("expected_targets_missing") or []
    assert sel in seen and not missing, (
        f"selector {sel!r} (synthesized form) must be seen, got "
        f"seen={seen} missing={missing}"
    )
    assert im.get("passes") is True, f"intent_match.passes should be true: {im}"
    print(f"OK  {sel!r} -> expected_targets_seen={seen}  intent_match.passes={im['passes']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
