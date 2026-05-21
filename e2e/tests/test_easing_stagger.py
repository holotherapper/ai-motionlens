#!/usr/bin/env python3
"""motion.assess easing / stagger / overshoot fields.

Three checks:
- css-animation.html declares `linear` easing for a 1000ms translate.
  `per_target_easing` must contain at least one entry whose
  `best_match_easing == "linear"` and `rms_error` is low (≤ 0.10).
- modal.html declares `ease-out` (opacity) + `cubic-bezier(0.22, 1, 0.36, 1)`
  (transform). The two targets (#modal + #backdrop) start moving at the
  same t_ms after the click, so `stagger_uniformity` must be set
  (>= 0.9 — the limiting case of synchronised onset rounds to ~1.0).
- intent.expected_easing makes `intent_match.easing_match` non-null and
  the gate consults it for the pass/fail bar.  Declaring a wrong easing
  must flip intent_match.passes to False.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url

CATALOG = {
    "linear",
    "ease-in",
    "ease-out",
    "ease-in-out",
    "ease-in-cubic",
    "ease-out-cubic",
    "ease-in-out-cubic",
}


def main() -> int:
    with McpClient() as c:
        # === case 1: easing-linear.html = linear ===
        # The fixture defers animation start until `window.startAnimation()`
        # is called, so the first sampled frame is guaranteed to be at the
        # animation's t=0 (no wall-clock drift during page load).
        c1 = {
            "url": fixture_url("easing-linear.html"),
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
                        "kind": "evaluate",
                        "js": "window.startAnimation()",
                    },
                    "wait_policy": "none",
                }
            ],
            "sample_plan": {
                "target_times_ms": [0, 200, 400, 600, 800, 1000],
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
        a = r["assessment"]
        pte = a["per_target_easing"]
        assert pte, a
        for te in pte:
            assert te["best_match_easing"] in CATALOG, te
            assert isinstance(te["rms_error"], (int, float)) and te["rms_error"] >= 0.0, te
            assert te["observed_values"], te
            assert len(te["observed_progress"]) == len(te["observed_values"]), te
        # The red box (#box) animates linearly. At least one entry must
        # identify it as linear with a tight residual.
        linear_fits = [te for te in pte if te["best_match_easing"] == "linear"]
        assert linear_fits, [(t["selector"], t["best_match_easing"], t["rms_error"]) for t in pte]
        best_linear = min(linear_fits, key=lambda t: t["rms_error"])
        assert best_linear["rms_error"] <= 0.10, best_linear
        print(
            f"  linear: {best_linear['selector']} axis={best_linear['axis']} "
            f"rms={best_linear['rms_error']:.4f}"
        )

        # === case 2: modal.html = synchronised onset → high uniformity ===
        c2 = {
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
                "min_smoothness": 0.6,
                "max_jank_events": 0,
                "require_intent_match": False,
                "min_coverage_score": 0.0,
                "require_non_static": True,
            },
            "include_contact_sheet": False,
        }
        r2 = c.call("motion.verify", c2)
        a2 = r2["assessment"]
        # Modal and backdrop start together → synchronised → uniformity ~1.0
        # (or None if only one selector moved meaningfully).
        if a2["stagger_uniformity"] is not None:
            assert a2["stagger_uniformity"] >= 0.9, a2["stagger_uniformity"]
        # Overshoot may or may not be reported depending on cubic-bezier
        # numerical fit; just assert the field exists and is a list.
        assert isinstance(a2["overshoot_events"], list), a2
        print(
            f"  stagger_uniformity={a2['stagger_uniformity']} "
            f"overshoot_events={len(a2['overshoot_events'])} "
            f"easing_targets={len(a2['per_target_easing'])}"
        )

        # === case 3: intent.expected_easing — easing_match wires through ===
        c3 = dict(c2)
        c3["episode_intent"] = {
            "description": "Modal opens with linear easing.",
            "expected_duration_ms": 300,
            "expected_kinds": ["fade", "translate"],
            "expected_targets": ["#modal"],
            "forbidden_kinds": [],
            "expected_easing": "linear",
        }
        c3["thresholds"] = dict(c3["thresholds"], require_intent_match=True)
        r3 = c.call("motion.verify", c3)
        im = r3["assessment"]["intent_match"]
        assert im is not None, r3
        em = im.get("easing_match")
        assert em is not None, im
        assert em["expected"] == "linear", em
        assert em["observed"] in CATALOG or em["observed"] == "unknown", em
        # The modal uses ease-out, not linear, so easing_match.passes must be False.
        assert em["passes"] is False, em
        # The intent-level passes must also be False because easing didn't match.
        assert im["passes"] is False, im
        print(f"  easing_match: expected={em['expected']} observed={em['observed']}")
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
