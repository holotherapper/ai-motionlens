#!/usr/bin/env python3
"""Regression guard: `MotionContract.include_final_fullpage_screenshot: true`
attaches a fullpage PNG of the page's settled state to the
`MotionVerifyReport.final_fullpage_screenshot` field. Agents who need
"how does the whole rendered page look at the end" can read this
artefact through the gate instead of falling back to
`browser_take_screenshot`.
"""
from __future__ import annotations

import os
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    base = {
        # scroll-document.html runs taller than the viewport, so the
        # fullpage screenshot will exceed `viewport.height`.
        "url": fixture_url("scroll-document.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "sample_plan": {"target_times_ms": [0, 250, 500, 750, 1000], "include_layout": True},
        "thresholds": {
            "min_smoothness": 0.0,
            "max_jank_events": 99,
            "require_intent_match": False,
            "min_coverage_score": 0.0,
            "require_non_static": False,
        },
        "include_contact_sheet": False,
    }

    # Default: no fullpage screenshot.
    with McpClient() as c:
        r_default = c.call("motion.verify", base)
    assert r_default.get("final_fullpage_screenshot") in (None, {}), (
        f"final_fullpage_screenshot must be absent by default; "
        f"got {r_default.get('final_fullpage_screenshot')}"
    )

    # With the flag: an artefact comes back, the file exists, and
    # it's taller than the viewport (= really a fullpage shot).
    contract = dict(base, include_final_fullpage_screenshot=True)
    with McpClient() as c:
        r = c.call("motion.verify", contract)
    art = r.get("final_fullpage_screenshot") or {}
    assert art, (
        f"final_fullpage_screenshot must be populated when the flag "
        f"is on; got {r.get('final_fullpage_screenshot')}"
    )
    p = art.get("artifact_local_path")
    assert p and os.path.exists(p), (
        f"final_fullpage_screenshot.artifact_local_path must exist on "
        f"disk; got {p}"
    )
    h = art.get("height", 0)
    assert h > 800, (
        f"a fullpage shot of scroll-document.html should be taller "
        f"than the 800px viewport; got height={h}"
    )
    print(
        f"OK  final_fullpage_screenshot attached "
        f"(path={p}, size={art.get('width')}x{h}); the gate now "
        f"obviates browser_take_screenshot for end-state visual "
        f"review"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
