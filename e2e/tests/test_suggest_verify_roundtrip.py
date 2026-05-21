#!/usr/bin/env python3
"""Regression guard: `motion.suggest_intent` must classify a canonical
hero entrance (initial `opacity:0` + `translateY`, fading to opacity:1)
the same way `motion.verify` classifies it once the suggestion is fed
back as `expected_kinds`. The two tools share `classify_transition`,
which consults `layout_snapshot.hidden_selectors` so a selector
that was filtered out at t=0 (opacity:0) and visible at t=600 is
labelled `fade`, not `appearance`. Otherwise the same animation reads
as `appearance + translate + fade` under suggest_intent and
`fade + translate` under verify, breaking the suggest → paste → verify
loop the skill explicitly recommends.

Steps:
1. Run `motion.suggest_intent` against the hero-entrance fixture.
2. Assert `detected_motion_kinds` does NOT contain "appearance" — the
   element is fading in, not materialising.
3. Feed the suggested kinds + targets straight into a `motion.verify`
   contract and assert it passes (intent-match correctness gate).
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    url = fixture_url("suggest-verify-roundtrip.html")

    with McpClient() as c:
        suggest = c.call(
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
            },
        )

    kinds = suggest.get("detected_motion_kinds") or []
    assert "appearance" not in kinds, (
        f"a hero entrance starting at opacity:0 must be classified as "
        f"`fade`, not `appearance` — the visibility filter dropping the "
        f"element at t=0 should be reconciled via hidden_selectors. "
        f"got detected_motion_kinds={kinds}"
    )
    # We expect at least `fade` for the opacity 0→1 leg. `translate` may
    # or may not be picked up depending on whether the probe samples
    # straddle the 12px translateY; we don't pin it.
    assert "fade" in kinds, (
        f"the opacity 0→1 leg must be picked up as `fade`; "
        f"got detected_motion_kinds={kinds}"
    )

    suggested = suggest["suggested_intent"]
    targets = suggested.get("expected_targets") or []
    assert targets, f"suggest_intent must surface a moved selector; got {targets}"

    contract = {
        "url": url,
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": suggested.get("description", "suggested"),
            "expected_duration_ms": suggested.get("expected_duration_ms") or 600,
            "expected_kinds": kinds,
            "expected_targets": targets,
        },
        "sample_plan": {"target_times_ms": [0, 150, 300, 450, 600], "include_layout": True},
        "thresholds": {
            "min_smoothness": 0.0,
            "max_jank_events": 99,
            "require_intent_match": True,
            "min_coverage_score": 0.0,
            "require_non_static": True,
        },
        "include_contact_sheet": False,
    }
    with McpClient() as c:
        verify = c.call("motion.verify", contract)

    im = (verify.get("assessment") or {}).get("intent_match") or {}
    assert im.get("passes") is True, (
        f"feeding suggest_intent output straight into verify must pass "
        f"intent-match (the two tools share classify_transition + "
        f"hidden_selectors); got intent_match={im}, "
        f"failed_gates={verify.get('failed_gates')}"
    )
    print(
        f"OK  suggest_intent.kinds={kinds} round-trips through motion.verify "
        f"(intent_match.passes=true) — appearance/fade classifier is "
        f"independent of expected_targets"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
