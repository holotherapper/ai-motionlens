#!/usr/bin/env python3
"""Regression guard: a `sample_plan.target_times_ms` entry at the same
virtual time as a CDP trigger does not nuke `motion.verify`.

A CDP trigger auto-advances the virtual clock by ~16ms (paint flush).
Without this guard, a sample target at the trigger's `at_t_ms` would be
`at_t < now` after the flush and the scheduler would return
`BackwardSeekUnsupported`, killing the whole verify run even though the
contract was otherwise valid.

The fix is `at_or_after` semantics on the Capture path: capture at the
current time and record `sample_plan_drift_after_cdp_trigger: ...` in
`evidence_missing`. A Trigger going backwards is still a real contract
bug and still hard-errors — only Capture is rescued.

hover-css.html has a `.target` whose `:hover` activates only under CDP
input. The contract places a `target_times_ms` sample at the SAME virtual
time as a CDP hover trigger, so the drift path is forced.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    contract = {
        "url": fixture_url("hover-css.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": "CDP hover translates a target on :hover.",
            "expected_duration_ms": 300,
            "expected_kinds": ["translate"],
            "expected_targets": ["#target"],
        },
        "triggers": [
            {
                "at_t_ms": 200,
                "kind": {"kind": "hover", "target": {"selector": "#target", "frame_path": []}},
                "input_mode": "cdp",
                "wait_policy": "none",
            }
        ],
        # 200 collides with the trigger; after the +16ms CDP paint flush
        # this sample is in the past — exactly the case the at_or_after
        # rescue covers so the whole verify run isn't killed with
        # BackwardSeekUnsupported.
        "sample_plan": {"target_times_ms": [0, 200, 400, 800], "include_layout": True},
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
        # Must NOT raise. Without the at_or_after rescue this would raise
        # a RuntimeError with BackwardSeekUnsupported and nothing else
        # would come back.
        r = c.call("motion.verify", contract)

    # The drift is explained inline.
    em = r.get("evidence_missing") or []
    assert any("sample_plan_drift_after_cdp_trigger" in e for e in em), (
        f"expected sample_plan_drift_after_cdp_trigger note in evidence_missing, got {em}"
    )

    # All four target points produced a frame (the drifted one captured
    # at_or_after the trigger time, not skipped).
    frames = r.get("frames") or []
    assert len(frames) == 4, f"expected 4 captures (incl. the rescued one), got {len(frames)}"
    ts = sorted(f["t_ms"] for f in frames)
    # The collided sample sits at ~216ms (trigger 200 + 16ms paint flush)
    # rather than the requested 200 — but it IS captured.
    near_trigger = [t for t in ts if 200.0 <= t <= 260.0]
    assert near_trigger, f"the collided sample was not rescued; t_ms list={ts}"
    print(
        f"OK  t_ms={[round(t,1) for t in ts]}  "
        f"drift_note={[e for e in em if 'drift' in e][:1]}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
