#!/usr/bin/env python3
"""motion.suggest_intent: probe a page, return an EpisodeIntent draft.

Two cases:
- modal.html with a click trigger: the response must classify motion
  (fade / translate among detected_motion_kinds), surface `#modal` as a
  moved selector, and propose a non-trivial duration.
- broken-layout.html with no triggers: the page is static, so is_static
  must be True, the draft must declare no kinds / targets, and notes
  must steer the agent towards expanding the probe or supplying a
  trigger.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    with McpClient() as c:
        # === case 1: probe modal — must detect motion and propose a draft ===
        req_pass = {
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
            "probe_window_ms": 480,
            "probe_steps": 6,
        }
        r = c.call("motion.suggest_intent", req_pass)
        assert r["is_static"] is False, r
        assert r["detected_motion_kinds"], r
        # The modal fixture animates fade + translate; the probe must catch
        # at least one of those kinds.
        kinds = set(r["detected_motion_kinds"])
        assert kinds & {"fade", "translate"}, kinds
        # #modal is the moving target.  Allow nested selectors (#modal > *)
        # as long as something containing "modal" was identified.
        assert any("modal" in s for s in r["moved_selectors"]), r["moved_selectors"]
        # The draft must round-trip into an EpisodeIntent: description set,
        # kinds + targets copied, duration suggested.
        si = r["suggested_intent"]
        assert isinstance(si["description"], str) and si["description"], si
        assert si["expected_kinds"] == r["detected_motion_kinds"], si
        assert si["expected_targets"] == r["moved_selectors"], si
        assert si["expected_duration_ms"] is None or si["expected_duration_ms"] > 0, si
        print(
            f"  modal: kinds={r['detected_motion_kinds']} "
            f"targets[:3]={r['moved_selectors'][:3]} "
            f"duration={si['expected_duration_ms']} smoothness={r['smoothness']:.2f}"
        )

        # === case 2: static page — must report is_static + empty draft ===
        req_static = {
            "url": fixture_url("broken-layout.html"),
            "viewport": {
                "width": 1280,
                "height": 800,
                "device_scale_factor": 1.0,
                "headless": True,
            },
            "probe_window_ms": 200,
            "probe_steps": 4,
        }
        r2 = c.call("motion.suggest_intent", req_static)
        assert r2["is_static"] is True, r2
        assert r2["detected_motion_kinds"] == [], r2
        assert r2["moved_selectors"] == [], r2
        si2 = r2["suggested_intent"]
        assert si2["expected_kinds"] == [], si2
        assert si2["expected_targets"] == [], si2
        assert si2["expected_duration_ms"] is None, si2
        # Notes must mention the static observation so the AI knows to
        # widen the probe / supply a trigger / try a different URL.
        assert any("static" in n.lower() or "no motion" in n.lower() for n in r2["notes"]), r2[
            "notes"
        ]
        print(f"  static: is_static=True notes_count={len(r2['notes'])}")
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
