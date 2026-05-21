#!/usr/bin/env python3
"""Regression guard: an `expected_targets` selector that starts at
opacity:0 (or display:none, or zero bbox) is still observed through the
sequence — the layout probe's visibility filter is bypassed for
elements named in MATCH_SELECTORS. Without the bypass, the canonical
hero entrance pattern drops out of the first layout snapshot and is
reported as selector-not-found.

entrance-from-zero.html arms a `.eyebrow` element at opacity:0 and
fades it in via @keyframes. Without the bypass, the t=0 snapshot drops
the element entirely and intent_match misses it.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    contract = {
        "url": fixture_url("entrance-from-zero.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": "Eyebrow fades in over 600ms from opacity 0.",
            "expected_duration_ms": 600,
            "expected_kinds": ["fade", "translate"],
            "expected_targets": ["#eyebrow"],
        },
        "sample_plan": {"target_times_ms": [0, 200, 400, 600], "include_layout": True},
        "thresholds": {
            "min_smoothness": 0.0,
            "max_jank_events": 99,
            "require_intent_match": True,
            "min_coverage_score": 0.0,
            "require_non_static": True,
        },
        "include_contact_sheet": False,
    }
    with McpClient() as c:
        r = c.call("motion.verify", contract)

    im = (r.get("assessment") or {}).get("intent_match") or {}
    seen = im.get("expected_targets_seen") or []
    missing = im.get("expected_targets_missing") or []
    assert "#eyebrow" in seen and not missing, (
        f"the opacity:0-at-t=0 eyebrow must still be observed via the "
        f"MATCH_SELECTORS bypass; got seen={seen} missing={missing}"
    )
    assert im.get("passes") is True, f"intent_match.passes should be true: {im}"
    print(f"OK  #eyebrow seen from opacity:0 via match_selectors bypass; intent passes")
    return 0


if __name__ == "__main__":
    sys.exit(main())
