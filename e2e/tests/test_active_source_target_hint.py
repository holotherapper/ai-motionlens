#!/usr/bin/env python3
"""Regression guard: `active_sources[].target_selector_hint` resolves
hashed CDP `cssId`s back to a human-readable selector by cross-
referencing the source's `name` with
`layout_snapshot.elements[].running_animations`. Singleton matches
(one element runs the named animation) get a hint; staggered shared
names (many `.word` elements share `fade-up`) intentionally stay
None so the agent doesn't misread an arbitrary pick as authoritative.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    # Use the existing entrance-from-zero fixture: it animates a
    # single `#eyebrow` element with `@keyframes fade-in`. The id is
    # stable so cssId might be `#eyebrow` (no hint needed) OR a hash
    # (depending on Chromium behaviour) — either way running_animations
    # carries the name, and a singleton resolves cleanly.
    contract = {
        "url": fixture_url("entrance-from-zero.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": "Eyebrow fades in.",
            "expected_duration_ms": 600,
            "expected_kinds": ["fade"],
            "expected_targets": ["#eyebrow"],
        },
        "sample_plan": {"target_times_ms": [0, 150, 300, 450, 600], "include_layout": True},
        "thresholds": {
            "min_smoothness": 0.0,
            "max_jank_events": 99,
            "require_intent_match": False,
            "min_coverage_score": 0.0,
            "require_non_static": False,
        },
        # Keep frame detail off (default) — the hint must still resolve.
        "include_contact_sheet": False,
    }

    with McpClient() as c:
        r = c.call("motion.verify", contract)

    intervals = r.get("unobserved_intervals") or []
    found_hint = False
    for iv in intervals:
        for s in (iv.get("active_sources") or []):
            if s.get("name") == "fade-in":
                hint = s.get("target_selector_hint")
                if hint:
                    assert "eyebrow" in hint, (
                        f"target_selector_hint for `fade-in` should "
                        f"point at the eyebrow element; got {hint!r}"
                    )
                    found_hint = True

    assert found_hint, (
        f"target_selector_hint must be resolved for the singleton "
        f"`fade-in` source — running_animations cross-reference "
        f"should have surfaced an eyebrow selector; intervals="
        f"{intervals}"
    )
    print(
        f"OK  active_source.name='fade-in' resolved its "
        f"target_selector_hint to the eyebrow element even with "
        f"include_frame_detail=false"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
