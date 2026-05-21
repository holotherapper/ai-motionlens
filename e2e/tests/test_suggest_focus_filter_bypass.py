#!/usr/bin/env python3
"""Regression guard: `motion.suggest_intent` with a `focus_selectors`
that points at an `opacity:0`-armed hero entrance must classify it as
real motion (NOT `is_static: true`). The layout probe's visibility
filter would otherwise drop the element from the t=0 sample, and a
probe that never sees the entrance baseline reports the whole window
as static — leading an agent to misread `is_static: true` as "the
element is not animating".

The fix forwards `focus_selectors` to the probe's `match_selectors`
list, granting it the same visibility-filter bypass `motion.verify`
already gives to `expected_targets`. `focus_selectors` is an
observer-named "watch these elements" hint and must play the same role
in both tools.

entrance-from-zero.html keeps a `#eyebrow` element at opacity:0 and
fades it in over 600ms.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    url = fixture_url("entrance-from-zero.html")

    with McpClient() as c:
        r = c.call(
            "motion.suggest_intent",
            {
                "url": url,
                "viewport": {
                    "width": 1280,
                    "height": 800,
                    "device_scale_factor": 1.0,
                    "headless": True,
                },
                "probe_window_ms": 600,
                "probe_steps": 6,
                "focus_selectors": ["#eyebrow"],
            },
        )

    assert r.get("is_static") is False, (
        f"focus_selectors must bypass the visibility filter for "
        f"opacity:0-armed targets so the probe sees the fade-in; "
        f"got is_static={r.get('is_static')}, notes={r.get('notes')}"
    )
    kinds = r.get("detected_motion_kinds") or []
    assert "fade" in kinds, (
        f"the focused opacity:0→1 element must be classified as `fade` "
        f"once the visibility filter is bypassed; got "
        f"detected_motion_kinds={kinds}"
    )
    moved = r.get("moved_selectors") or []
    assert any("eyebrow" in s for s in moved), (
        f"moved_selectors must include the focused target; got {moved}"
    )
    print(
        f"OK  focus_selectors=['#eyebrow'] sees the opacity:0 fade-in "
        f"(is_static=false, kinds={kinds}, moved={moved})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
