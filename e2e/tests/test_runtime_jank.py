#!/usr/bin/env python3
"""motion.assess runtime_jank_events shape.

LoAF entries are emitted by Chromium's PerformanceObserver under
`long-animation-frame`.  They surface real-time jank — main-thread
blocking longer than ~50ms during a frame.  Under a fully paused
virtual clock no entries normally fire (the renderer doesn't tick),
so the canonical assertion here is: the field exists, is a list,
and each entry follows the schema. When entries do fire (typically
during `clock.advance` budget burn) we additionally assert positive
durations.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    with McpClient() as c:
        contract = {
            "url": fixture_url("modal.html"),
            "viewport": {
                "width": 1280,
                "height": 800,
                "device_scale_factor": 1.0,
                "headless": True,
            },
            "triggers": [
                {
                    "at_t_ms": 0,
                    "kind": {
                        "kind": "click",
                        "target": {
                            "selector": "#open-modal",
                            "frame_path": [],
                            "resolved_coordinates": None,
                        },
                    },
                    "wait_policy": "none",
                }
            ],
            "sample_plan": {
                "target_times_ms": [0, 60, 150, 240, 320],
                "include_layout": True,
            },
            "thresholds": {
                "min_smoothness": 0.2,
                "max_jank_events": 5,
                "require_intent_match": False,
                "min_coverage_score": 0.0,
                "require_non_static": False,
            },
            "include_contact_sheet": False,
        }
        r = c.call("motion.verify", contract)
        assessment = r["assessment"]
        assert "runtime_jank_events" in assessment, assessment.keys()
        events = assessment["runtime_jank_events"]
        assert isinstance(events, list), events
        required_keys = {
            "start_time_ms",
            "duration_ms",
            "render_start_ms",
            "style_and_layout_start_ms",
            "blocking_duration_ms",
        }
        for e in events:
            missing = required_keys - set(e.keys())
            assert not missing, (missing, e)
            assert isinstance(e["duration_ms"], (int, float)), e
            assert e["duration_ms"] >= 0.0, e
            assert e["blocking_duration_ms"] >= 0.0, e
        print(f"  runtime_jank_events: {len(events)} entries")
        if events:
            print(
                f"    first: start={events[0]['start_time_ms']:.1f}ms "
                f"duration={events[0]['duration_ms']:.1f}ms "
                f"blocking={events[0]['blocking_duration_ms']:.1f}ms"
            )
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
