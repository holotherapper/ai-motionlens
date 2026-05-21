#!/usr/bin/env python3
"""Regression tests for two verified motion.verify fixes.

Fix 1 — intent.expected_targets is graded by real CSS `Element.matches()`,
        not by string-equality against the synthesized `#id`-priority
        notation. A flawless animation must NOT fail just because the
        contract used a valid-but-differently-spelled selector.
        easing-linear.html's box is `<div class="box" id="box">`; the
        contract intentionally uses `div.box` (which does not string-equal
        the tool's synthesized `#box`).

Fix 3 — jank detection catches a sudden positional discontinuity
        (an element teleporting in one interval) from the bbox series,
        not only from the whole-page pixel-delta CV. jank.html jumps
        left 200px -> 700px at the 20% mark; jank_events must be non-empty.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    with McpClient() as c:
        # === Fix 1: CSS-semantic expected_targets matching ===
        contract_sel = {
            "url": fixture_url("easing-linear.html"),
            "viewport": {
                "width": 1280,
                "height": 800,
                "device_scale_factor": 1.0,
                "headless": True,
            },
            "episode_intent": {
                "description": "Red box translates 0->1000px over 1000ms linear.",
                "expected_duration_ms": 1000,
                "expected_kinds": ["translate"],
                # Valid CSS selector for <div class="box" id="box">, but it
                # does NOT string-equal the synthesized `#box`. Before the
                # fix this made a perfect animation fail.
                "expected_targets": ["div.box"],
                "expected_easing": "linear",
            },
            "triggers": [
                {
                    "at_t_ms": 0,
                    "kind": {
                        "kind": "evaluate",
                        "js": "window.startAnimation()",
                        "frame_path": [],
                    },
                    "wait_policy": "none",
                }
            ],
            "sample_plan": {
                "target_times_ms": [0, 100, 300, 500, 700, 1000],
                "include_layout": True,
            },
            "thresholds": {
                "min_smoothness": 0.6,
                "max_jank_events": 0,
                "require_intent_match": True,
                "min_coverage_score": 0.5,
                "require_non_static": True,
            },
            "include_contact_sheet": False,
        }
        r = c.call("motion.verify", contract_sel)
        im = r["assessment"]["intent_match"]
        if r["passes"] is not True:
            print("FAIL selector-fix report dump:")
            print(f"  verdict_human={r['verdict_human']}")
            print(f"  intent_match={im}")
            print(f"  evidence_missing={r['evidence_missing']}")
        assert r["passes"] is True, (
            f"a flawless animation graded by CSS selector `div.box` must pass; "
            f"got passes={r['passes']} verdict_human={r['verdict_human']}"
        )
        assert im is not None and im["passes"] is True, im
        assert "div.box" in im["expected_targets_seen"], (
            f"`div.box` must be recognized via Element.matches(); "
            f"seen={im['expected_targets_seen']} missing={im['expected_targets_missing']}"
        )
        assert not im["expected_targets_missing"], im["expected_targets_missing"]
        print(
            f"  Fix1 selector: passes={r['passes']} "
            f"targets_seen={im['expected_targets_seen']}"
        )

        # === Fix 3: positional-discontinuity jank detection ===
        contract_jank = {
            "url": fixture_url("jank.html"),
            "viewport": {
                "width": 1280,
                "height": 800,
                "device_scale_factor": 1.0,
                "headless": True,
            },
            "episode_intent": {
                "description": "Box moves left->right over 1000ms; should be smooth.",
                "expected_duration_ms": 1000,
                "expected_kinds": ["translate"],
                "expected_targets": [],
            },
            "sample_plan": {
                "target_times_ms": [0, 100, 180, 195, 205, 220, 300, 500, 1000],
                "include_layout": True,
            },
            "thresholds": {
                "min_smoothness": 0.6,
                "max_jank_events": 0,
                "require_intent_match": False,
                "min_coverage_score": 0.0,
                "require_non_static": True,
            },
            "include_contact_sheet": False,
        }
        rj = c.call("motion.verify", contract_jank)
        jank_events = rj["assessment"]["jank_events"]
        if not jank_events:
            print("FAIL jank-fix report dump:")
            print(f"  verdict_human={rj['verdict_human']}")
            print(f"  smoothness={rj['assessment']['smoothness']:.3f}")
            print(f"  jank_events={jank_events}")
        assert len(jank_events) >= 1, (
            "the 200px->700px teleport in jank.html must be caught by the "
            f"positional jank detector; jank_events={jank_events}"
        )
        assert rj["passes"] is False, (
            f"a janky page must not pass; got passes={rj['passes']}"
        )
        print(
            f"  Fix3 jank: jank_events={len(jank_events)} "
            f"passes={rj['passes']} verdict={rj['verdict']}"
        )
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
