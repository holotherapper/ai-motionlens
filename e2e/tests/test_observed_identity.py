#!/usr/bin/env python3
"""Regression guard: motion.verify stamps the report with what the URL
ACTUALLY served (document.title + location.href), so a port collision /
redirect / wrong build at the same URL is visible at a glance instead of
being mistaken for an animation defect.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    url = fixture_url("css-animation.html")
    contract = {
        "url": url,
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": "Box translates on load.",
            "expected_duration_ms": 1000,
            "expected_kinds": ["translate"],
            "expected_targets": [],
        },
        "sample_plan": {"target_times_ms": [0, 250, 500, 1000], "include_layout": True},
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

    assert r.get("observed_url"), f"missing observed_url: {r.get('observed_url')!r}"
    assert r.get("observed_title") is not None, (
        f"missing observed_title (None vs empty: {r.get('observed_title')!r})"
    )

    # observed_url should resolve to the same file we asked for.
    assert r["observed_url"].endswith("css-animation.html"), (
        f"observed_url did not land on the requested fixture: {r['observed_url']!r}"
    )
    # contract_url and observed_url should agree on the requested page —
    # a divergence here is exactly the port-collision / redirect signal
    # the field exists to surface.
    assert r["contract_url"].endswith("css-animation.html"), r["contract_url"]

    print(f"OK  observed_title={r['observed_title']!r}  observed_url={r['observed_url']!r}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
