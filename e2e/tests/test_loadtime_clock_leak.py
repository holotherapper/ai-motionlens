#!/usr/bin/env python3
"""Regression guard: load-time auto-start animation observed from true t=0.

css-animation.html applies `animation: move 1000ms linear` on load
(no trigger), translating a box left 0 -> 1000px. The virtual clock is
paused the instant navigation commits, so only the irreducible
nav-commit time (~10ms) leaks into the document timeline.

If wall-clock leaks before the pause, the box is already mid-flight at
virtual t=0 (parked near x~703) and the whole sequence is observed as
only a few px of motion near the end. This test asserts the box starts
near 0 at t=0 and progresses monotonically across the full 0..1000px
range.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    contract = {
        "url": fixture_url("css-animation.html"),
        "viewport": {
            "width": 1280,
            "height": 800,
            "device_scale_factor": 1.0,
            "headless": True,
        },
        "episode_intent": {
            "description": "Box translates left 0->1000px over 1000ms linear, on load.",
            "expected_duration_ms": 1000,
            "expected_kinds": ["translate"],
            "expected_targets": [],
        },
        "sample_plan": {
            "target_times_ms": [0, 250, 500, 750, 1000],
            "include_layout": True,
        },
        "thresholds": {
            "min_smoothness": 0.6,
            "max_jank_events": 99,
            "require_intent_match": False,
            "min_coverage_score": 0.0,
            "require_non_static": True,
        },
        "include_contact_sheet": False,
        # This guard reads frames[].layout_snapshot directly to prove the
        # box progresses from true t=0. layout_snapshot is stripped from the
        # response by default, so opt in. (The strip behaviour itself is
        # covered by test_frame_detail_strip.py.)
        "include_frame_detail": True,
    }
    with McpClient() as c:
        r = c.call("motion.verify", contract)

    xs = []
    for f in r["frames"]:
        ls = f.get("layout_snapshot") or {}
        bx = None
        for el in ls.get("elements", []) or []:
            if "box" in (el.get("selector") or ""):
                bb = el.get("bbox")
                if bb:
                    bx = bb[0]
        xs.append((f["t_ms"], bx))

    by_t = {t: x for t, x in xs if x is not None}
    for t in (0.0, 250.0, 500.0, 1000.0):
        assert t in by_t, f"missing sample t={t}: {xs}"

    # t=0 must be near the start (left:0); a pre-pause leak would park it ~703.
    assert by_t[0.0] < 80.0, (
        f"load-time animation observed mid-flight at t=0 (box_x={by_t[0.0]:.1f}, "
        f"expected <80 ~ left:0). The pre-pause wall-clock leak has regressed. {xs}"
    )
    # Full-range linear progression, not a few px around 700-1000.
    assert by_t[1000.0] > 900.0, f"end not reached: {xs}"
    assert by_t[0.0] < by_t[250.0] < by_t[500.0] < by_t[1000.0], (
        f"box_x must increase monotonically across the full range: {xs}"
    )
    span = by_t[1000.0] - by_t[0.0]
    assert span > 850.0, (
        f"observed translate span only {span:.1f}px (<850) — sequence is "
        f"mid-flight, not from true t=0: {xs}"
    )
    print(f"OK  (t_ms,box_x)={[(t, round(x,1)) for t,x in xs]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
