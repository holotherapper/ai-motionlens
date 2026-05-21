#!/usr/bin/env python3
"""Regression guard: silent-static-prominent-element catches an
EFFECTIVELY-INVISIBLE stuck element, not just an area-prominent one.

An area-only floor (flagging elements >=6% of the viewport AND
completely still) is not enough: a hero whose lines are stuck at
`translateY(342px)` under an `overflow:hidden` mask is small by area
and offset out of its visible band, so it slips the net and a
main-element silent break passes with no hint. The detector adds an
"effectively invisible" path: an element offset mostly outside the
viewport that never moved, while others did, is flagged regardless of
its area.

silent-offscreen.html: a ~1.4%-of-viewport heading (UNDER the 6% floor)
offset 2000px below the fold (never animates back), plus a box that DOES
translate on load. The hint must still fire, point at the heading, and
report `effectively_invisible: true` with `viewport_fraction < 0.06`
(proving the area floor alone would have missed it).
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    contract = {
        "url": fixture_url("silent-offscreen.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": "Box translates on load; heading is NOT declared.",
            "expected_duration_ms": 1000,
            "expected_kinds": ["translate"],
            "expected_targets": ["div.box"],  # heading deliberately undeclared
        },
        "sample_plan": {"target_times_ms": [0, 250, 500, 750, 1000], "include_layout": True},
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
        r = c.call("motion.verify", contract)

    hints = r.get("diagnosis_hints") or []
    ss = [h for h in hints if h.get("code") == "silent-static-prominent-element"]
    assert ss, (
        f"expected silent-static-prominent-element for an offscreen-stuck "
        f"heading; codes={[h.get('code') for h in hints]}"
    )
    h = next((x for x in ss if "headline" in (x.get("target_selector") or "")), None)
    assert h, f"hint must target the stuck heading, got {[x.get('target_selector') for x in ss]}"

    obs = h.get("observed") or {}
    assert obs.get("effectively_invisible") is True, (
        f"must be flagged via the effectively-invisible path: {obs}"
    )
    assert obs.get("viewport_fraction", 1.0) < 0.06, (
        f"heading is sub-6% by area — the area floor alone would have missed it; "
        f"got viewport_fraction={obs.get('viewport_fraction')}"
    )
    assert obs.get("moved_selectors_count", 0) >= 1, f"box moved: {obs}"
    msg = h.get("message") or ""
    assert "out of the viewport" in msg.lower() or "offset" in msg.lower(), (
        f"message must explain it is offset/clipped out of view: {msg!r}"
    )
    assert h.get("suggested_probe"), "hint needs a suggested_probe"

    print(
        f"OK  target={h['target_selector']!r}  "
        f"effectively_invisible={obs.get('effectively_invisible')}  "
        f"viewport_fraction={obs.get('viewport_fraction'):.4f}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
