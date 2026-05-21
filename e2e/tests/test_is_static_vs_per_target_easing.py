#!/usr/bin/env python3
"""Regression guard: `is_static` must be reconciled against the
DOM-truth observation in `per_target_easing`. When a small element
translates a meaningful distance on a large viewport, the page-wide
pixel-delta mean sits well under `NO_MOTION_FLOOR` and the first-pass
`is_static` reads true — but `per_target_easing` clearly fits the
axis with `mag > 1e-3`. The headline `is_static` should flip to false
so the `non-static` correctness gate doesn't fire on a motion the
report has already graded as passing.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    contract = {
        "url": fixture_url("small-element-motion.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": "A small chip translates 20px upward over 600ms.",
            "expected_duration_ms": 600,
            "expected_kinds": ["translate"],
            "expected_targets": ["#chip"],
        },
        "sample_plan": {
            "target_times_ms": [0, 150, 300, 450, 600],
            "include_layout": True,
        },
        "thresholds": {
            "min_smoothness": 0.0,
            "max_jank_events": 99,
            "require_intent_match": True,
            "min_coverage_score": 0.0,
            # The whole point of this regression: require_non_static
            # must NOT fire on a small-element-on-large-page motion
            # that per_target_easing has clearly captured.
            "require_non_static": True,
        },
        "include_contact_sheet": False,
    }

    with McpClient() as c:
        r = c.call("motion.verify", contract)

    assessment = r.get("assessment") or {}
    pte = assessment.get("per_target_easing") or []
    is_static = assessment.get("is_static")
    im = assessment.get("intent_match") or {}
    fg = r.get("failed_gates") or []

    # Precondition: per_target_easing must have fit the small chip's
    # translate. If it hasn't, the reconcile pass has nothing to work
    # with and this test isn't measuring what it should.
    pte_axes = [e.get("axis") for e in pte]
    assert any(ax and "translate" in ax for ax in pte_axes), (
        f"precondition: per_target_easing must fit a *translate* axis "
        f"on the chip; got axes={pte_axes}, per_target_easing={pte}"
    )

    # The actual fix: is_static must be reconciled to false.
    assert is_static is False, (
        f"is_static must be reconciled to false when per_target_easing "
        f"fit a translate axis (the page-wide pixel-delta under-counts "
        f"a small element); got is_static={is_static}, "
        f"per_target_easing axes={pte_axes}"
    )

    # And therefore no non-static correctness gate.
    non_static_gates = [g for g in fg if g.get("gate") == "non-static"]
    assert not non_static_gates, (
        f"non-static correctness gate must NOT fire when "
        f"per_target_easing observed real motion; got {non_static_gates}"
    )

    # Headline pass.
    assert r.get("passes") is True, (
        f"the whole report must pass — intent_match green, no "
        f"non-static gate, no correctness defects; got passes={r.get('passes')}, "
        f"intent_match={im}, failed_gates={fg}"
    )
    print(
        f"OK  small element translate: per_target_easing axes={pte_axes} "
        f"reconciles is_static={is_static}; no non-static gate; "
        f"passes=True"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
