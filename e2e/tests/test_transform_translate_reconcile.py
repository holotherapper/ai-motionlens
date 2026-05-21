#!/usr/bin/env python3
"""Regression guard: a CSS `@keyframes` animation that only changes
`transform: translateY(...)` (no opacity, no rotate, no scale) is
classified as `translate` and surfaces a `transform-translate-y` axis
in `per_target_easing` — even on a paused virtual clock where
`getComputedStyle().transform` does not commit the in-progress
interpolation.

Without the reconcile pass, `motion.verify` on the canonical "hero
word rise" pattern returns `per_target_easing` with `axis: opacity`
(or nothing, on a pure-transform fixture) and `expected_kinds:
["translate"]` lands in `expected_kinds_missing`, because the per-pair
classifier and the easing fitter both read raw transforms that hadn't
moved across virtual-clock samples.

The reconcile pass evaluates `el.getAnimations()[].effect.getKeyframes()`,
extracts the translate component when both endpoints are pure
translates, and synthesises an interpolated
`matrix(1, 0, 0, 1, tx, ty)` per sample — the same way opacity is
reconciled.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    contract = {
        "url": fixture_url("keyframe-translate-only.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": "Hero words rise from translateY(20px) to translateY(0) over 800ms.",
            "expected_duration_ms": 800,
            "expected_kinds": ["translate"],
            "expected_targets": [".word"],
        },
        "sample_plan": {
            "target_times_ms": [0, 200, 400, 600, 800],
            "include_layout": True,
        },
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
        r = c.call("motion.verify", contract)

    assessment = r.get("assessment") or {}
    detected = assessment.get("detected_motion_kinds") or []
    pte = assessment.get("per_target_easing") or []
    axes = [e.get("axis") for e in pte]
    im = assessment.get("intent_match") or {}

    assert any(ax and "translate" in ax for ax in axes), (
        f"per_target_easing must surface a `*translate*` axis after "
        f"transform reconciliation; got axes={axes}, "
        f"per_target_easing={pte}"
    )
    assert "translate" in detected, (
        f"detected_motion_kinds must include `translate` (either via "
        f"the per-pair classifier seeing interpolated transforms or "
        f"via the per_target_easing reconcile pass); got "
        f"detected={detected}, axes={axes}"
    )
    assert im.get("passes") is True, (
        f"intent_match must pass for "
        f"`expected_kinds: [translate]` on a pure-translate @keyframes; "
        f"got intent_match={im}, failed_gates={r.get('failed_gates')}"
    )
    print(
        f"OK  pure-translate @keyframes reconciled via getAnimations(); "
        f"axes={axes} detected={detected} intent_match.passes=true"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
