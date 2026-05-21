#!/usr/bin/env python3
"""motion.verify diagnosis_hints: root-cause candidates.

Three failure shapes:
- static page with no triggers → no-motion-source-registered
- intent declares a selector that does not exist → selector-not-found
- intent declares an existing selector that never moves → bbox-static-style-mutating
  or hidden-by-zero-opacity (depending on the fixture's resting state)
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def codes(hints: list[dict]) -> list[str]:
    return [h["code"] for h in hints]


def main() -> int:
    with McpClient() as c:
        # === case 1: static page → no-motion-source-registered ===
        c1 = {
            "url": fixture_url("broken-layout.html"),
            "viewport": {
                "width": 1280,
                "height": 800,
                "device_scale_factor": 1.0,
                "headless": True,
            },
            "sample_plan": {
                "target_times_ms": [0, 100, 200],
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
        r = c.call("motion.verify", c1)
        assert r["passes"] is False, r
        hs = r["diagnosis_hints"]
        cs = codes(hs)
        assert "no-motion-source-registered" in cs, cs
        ns = next(h for h in hs if h["code"] == "no-motion-source-registered")
        assert ns["suggested_probe"], ns
        assert ns["target_selector"] is None, ns
        print(f"  case 1 static: {cs}")

        # === case 2: intent declares a bogus selector ===
        c2 = {
            "url": fixture_url("modal.html"),
            "viewport": {
                "width": 1280,
                "height": 800,
                "device_scale_factor": 1.0,
                "headless": True,
            },
            "episode_intent": {
                "description": "Modal animates a non-existent element.",
                "expected_kinds": ["fade"],
                "expected_targets": ["#this-id-does-not-exist"],
                "forbidden_kinds": [],
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
                "min_smoothness": 0.6,
                "max_jank_events": 0,
                "require_intent_match": True,
                "min_coverage_score": 0.0,
                "require_non_static": True,
            },
            "include_contact_sheet": False,
        }
        r2 = c.call("motion.verify", c2)
        assert r2["passes"] is False, r2
        hs2 = r2["diagnosis_hints"]
        cs2 = codes(hs2)
        assert "selector-not-found" in cs2, cs2
        nf = next(h for h in hs2 if h["code"] == "selector-not-found")
        assert nf["target_selector"] == "#this-id-does-not-exist", nf
        assert nf["suggested_probe"] and "dom_query" in nf["suggested_probe"], nf
        # observed should record both first/last presence as False
        obs = nf["observed"]
        assert obs.get("present_in_first_frame") is False, obs
        assert obs.get("present_in_last_frame") is False, obs
        print(f"  case 2 bogus selector: {cs2}")

        # === case 3: intent declares static-page targets that never move ===
        # broken-layout.html contains an `h1` that exists but never animates,
        # so the gate must mark it as evidence_missing and emit either
        # hidden-by-zero-opacity (if opacity stayed 0) or
        # bbox-static-style-mutating (if any style changed) or
        # selector-not-found-equivalent diagnostics.  The minimum bar is:
        # the hint list is non-empty AND every hint that names a target
        # names the same `h1` that intent declared.
        c3 = {
            "url": fixture_url("broken-layout.html"),
            "viewport": {
                "width": 1280,
                "height": 800,
                "device_scale_factor": 1.0,
                "headless": True,
            },
            "episode_intent": {
                "description": "h1 should fade in.",
                "expected_duration_ms": 200,
                "expected_kinds": ["fade"],
                "expected_targets": ["h1"],
                "forbidden_kinds": [],
            },
            "sample_plan": {
                "target_times_ms": [0, 100, 200],
                "include_layout": True,
            },
            "thresholds": {
                "min_smoothness": 0.6,
                "max_jank_events": 0,
                "require_intent_match": True,
                "min_coverage_score": 0.0,
                "require_non_static": True,
            },
            "include_contact_sheet": False,
        }
        r3 = c.call("motion.verify", c3)
        assert r3["passes"] is False, r3
        hs3 = r3["diagnosis_hints"]
        assert hs3, r3
        # static + no triggers, so no-motion-source-registered is expected too.
        cs3 = codes(hs3)
        assert "no-motion-source-registered" in cs3, cs3
        # All hints carrying a selector must name something we declared.
        for h in hs3:
            if h["target_selector"] is not None:
                assert h["target_selector"] == "h1", h
        print(f"  case 3 declared static h1: {cs3}")
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
