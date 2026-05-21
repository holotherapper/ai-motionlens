#!/usr/bin/env python3
"""Regression guard: a `trigger.scroll` between sample frames must NOT
register as a positional-teleport jank or as a per-target overshoot
on every visible element. Bbox observations are viewport-relative; the
layout snapshot carries `scroll_y` so motion.assess can back the
scroll shift out and compare document-relative positions instead.
Without it, every reveal card surfaces `peak_progress=2.04`
immediately after a scrollY=900 trigger.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    contract = {
        "url": fixture_url("scroll-then-reveal.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": "Cards appear in viewport after scroll; they don't animate.",
            # No expected_kinds — the cards are intentionally static
            # on the document. We only care that motion.assess doesn't
            # invent false jank / overshoot from the scroll itself.
            "expected_kinds": [],
            "expected_targets": ["#card-1", "#card-2"],
        },
        "triggers": [
            {
                "at_t_ms": 0,
                "kind": {"kind": "scroll", "x": 0, "y": 900},
            }
        ],
        "sample_plan": {
            "target_times_ms": [0, 80, 160, 240, 320, 400, 480, 560],
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

    assessment = r.get("assessment") or {}
    overshoots = assessment.get("overshoot_events") or []
    jank = assessment.get("jank_events") or []

    # Cards are static on the document — no overshoot should fire.
    card_overshoots = [
        o for o in overshoots
        if o.get("selector") in ("#card-1", "#card-2", "div.card")
    ]
    assert not card_overshoots, (
        f"static-on-document cards must not surface overshoot events "
        f"just because a scroll trigger moved the viewport; got "
        f"{card_overshoots}"
    )

    # Positional-teleport jank from the scroll itself: also forbidden.
    teleport_jank = [j for j in jank if j.get("kind") == "positional-teleport"]
    assert not teleport_jank, (
        f"scroll-induced viewport shift must not surface as "
        f"positional-teleport jank; got {teleport_jank}"
    )

    # The layout snapshot must carry scroll_y so a scroll shift can be
    # backed out. Capture one frame manually and confirm.
    with McpClient() as c:
        sid = c.call(
            "session.launch",
            {
                "url": fixture_url("scroll-then-reveal.html"),
                "viewport_width": 1280, "viewport_height": 800, "headless": True,
            },
        )["session_id"]
        try:
            c.call("episode.start", {"session_id": sid})
            c.call("trigger.scroll", {"session_id": sid, "x": 0, "y": 900})
            f = c.call(
                "frame.capture",
                {"session_id": sid, "format": "png", "layout": {}},
            )
        finally:
            c.call("session.close", {"session_id": sid})

    snap = (f or {}).get("layout_snapshot") or {}
    assert snap.get("scroll_y") and snap["scroll_y"] >= 800, (
        f"layout_snapshot must carry scroll_y after a scroll trigger; "
        f"got scroll_y={snap.get('scroll_y')}"
    )
    print(
        f"OK  scroll trigger 900px → no false overshoot / no false "
        f"positional-teleport; layout_snapshot.scroll_y={snap['scroll_y']}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
