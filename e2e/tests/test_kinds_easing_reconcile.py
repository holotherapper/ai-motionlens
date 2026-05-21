#!/usr/bin/env python3
"""Regression guard for the `detected_motion_kinds` ↔ `per_target_easing`
reconcile pass.

`classify_transition` operates on adjacent frame pairs with a 0.5px
movement threshold. On a sharp CSS transition (80ms ease-out) where the
contract samples land in the curve's tail, every adjacent delta drops
below threshold (the curve looks like `-9.35, -9.74, -9.94, -10, -10`)
and the per-pair classifier never emits `translate`. Yet
`per_target_easing` correctly fits `axis: transform-translate-y` over
the full sequence because it compares min/max amplitude, not per-pair.

The reconcile pass folds per_target_easing axes back into
`detected_motion_kinds`. This test pins that behaviour: a contract that
declares `expected_kinds: ["translate"]` on a late-sampled hover must
pass intent-match.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    contract = {
        "url": fixture_url("hover-late-sample.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": "Card lifts -10px with a 80ms ease-out transition.",
            # Coverage straddles the transition's full window so
            # duration-match doesn't bias the result; the reconcile
            # pass is what we're guarding.
            "expected_duration_ms": 120,
            "expected_kinds": ["translate"],
            "expected_targets": ["#card"],
        },
        # Samples land in the curve's tail: every adjacent delta is
        # under classify_transition's 0.5px threshold, but the sequence
        # min/max is the full 10px (per_target_easing must report it).
        "sample_plan": {
            "target_times_ms": [60, 90, 120, 150, 180],
            "include_layout": True,
        },
        "thresholds": {
            "min_smoothness": 0.0,
            "max_jank_events": 99,
            "require_intent_match": True,
            "min_coverage_score": 0.0,
            # Late-tail sampling produces ~0 per-frame pixel deltas
            # (the visual motion has already settled) — that's exactly
            # the precondition for this regression. Don't gate on it.
            "require_non_static": False,
        },
        "include_contact_sheet": False,
    }

    with McpClient() as c:
        r = c.call("motion.verify", contract)

    assessment = r.get("assessment") or {}
    detected = assessment.get("detected_motion_kinds") or []
    pte = assessment.get("per_target_easing") or []
    pte_axes = [e.get("axis") for e in pte]
    im = assessment.get("intent_match") or {}

    assert any(ax and "translate" in ax for ax in pte_axes), (
        f"per_target_easing must catch translate-y over the full "
        f"sequence (precondition for the reconcile pass); got "
        f"axes={pte_axes}"
    )
    assert "translate" in detected, (
        f"detected_motion_kinds must include `translate` after the "
        f"reconcile pass, since per_target_easing fit translate-y on "
        f"the same sequence; got detected={detected}, axes={pte_axes}, "
        f"intent_match={im}"
    )
    assert im.get("passes") is True, (
        f"intent_match must pass for `expected_kinds: [translate]` once "
        f"the two pipelines agree; got intent_match={im}, "
        f"failed_gates={r.get('failed_gates')}"
    )
    print(
        f"OK  per_target_easing axes={pte_axes} fold into "
        f"detected_motion_kinds={detected}; intent_match.passes=true"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
