#!/usr/bin/env python3
"""Regression guard: page-wide ambient animation loops (marquees,
orb-float drifts, brand-mark spinners) are extracted from
`unobserved_intervals[].active_sources` and surfaced once in
`ambient_source_names`, so the MCP response doesn't carry the same
N ambient sources × M intervals payload bloat that triggers repeated
`response_truncated_for_mcp_limit` events. `include_frame_detail: true`
keeps the full expansion as a compatibility escape hatch.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    base_contract = {
        "url": fixture_url("ambient-plus-entrance.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": "Hero fade-in; ambient loops run separately.",
            "expected_duration_ms": 600,
            "expected_kinds": ["fade"],
            "expected_targets": ["#hero"],
        },
        "sample_plan": {
            "target_times_ms": [0, 100, 250, 400, 600],
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

    # Part 1: include_frame_detail: false (default).
    c1 = dict(base_contract, include_frame_detail=False)
    with McpClient() as c:
        r1 = c.call("motion.verify", c1)

    ambient = r1.get("ambient_source_names") or []
    assert ambient, (
        f"ambient_source_names must surface the page-wide loops; got {ambient}"
    )
    # We expect marquee + mark-spin + orb-float as the three ambient
    # @keyframes names from the fixture.
    expected_in_ambient = {"marquee", "mark-spin", "orb-float"}
    assert expected_in_ambient.issubset(set(ambient)), (
        f"ambient_source_names must include {expected_in_ambient}; "
        f"got {ambient}"
    )
    # `hero-enter` is interval-specific — it must NOT be in ambient.
    assert "hero-enter" not in ambient, (
        f"interval-specific source `hero-enter` must NOT be classified "
        f"as ambient; got {ambient}"
    )
    # Per-interval active_sources must not carry the ambient names.
    intervals = r1.get("unobserved_intervals") or []
    for iv in intervals:
        names = [s.get("name") for s in (iv.get("active_sources") or [])]
        for amb in expected_in_ambient:
            assert amb not in names, (
                f"unobserved_intervals[].active_sources still contains "
                f"ambient `{amb}`; got {names} in interval {iv.get('interval')}"
            )

    # Part 2: include_frame_detail: true keeps the full expansion.
    c2 = dict(base_contract, include_frame_detail=True)
    with McpClient() as c:
        r2 = c.call("motion.verify", c2)
    ambient_full = r2.get("ambient_source_names") or []
    assert not ambient_full, (
        f"include_frame_detail: true must keep the legacy contract — "
        f"no ambient stripping; got ambient_source_names={ambient_full}"
    )
    # And the ambient names should still appear in the per-interval
    # active_sources lists.
    saw_ambient_in_intervals = False
    for iv in (r2.get("unobserved_intervals") or []):
        names = [s.get("name") for s in (iv.get("active_sources") or [])]
        if any(amb in names for amb in expected_in_ambient):
            saw_ambient_in_intervals = True
            break
    assert saw_ambient_in_intervals, (
        f"include_frame_detail: true must preserve the full ambient "
        f"expansion on at least one interval; got intervals "
        f"{r2.get('unobserved_intervals')}"
    )

    print(
        f"OK  ambient sources extracted: {ambient} (interval-specific "
        f"hero-enter preserved); include_frame_detail:true keeps "
        f"the legacy full expansion."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
