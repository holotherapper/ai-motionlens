#!/usr/bin/env python3
"""Regression guard: `intent_match.duration_match.observed_coverage_ms`
measures the expected_targets' actual onset→settle span, not the
whole `sample_plan` length. An ambient loop animation (a long-running
@keyframes drift, an infinite marquee) shouldn't be able to inflate
the verdict and false-fail `duration_match` as `too-long`: when an
agent extends `target_times_ms` to capture the reveal, the ambient
blob must not eat the coverage budget.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    contract = {
        "url": fixture_url("target-with-ambient-loop.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": "The chip rises 20px in 600ms; ambient blob loops separately.",
            "expected_duration_ms": 600,
            "expected_kinds": ["translate"],
            "expected_targets": ["#chip"],
        },
        # Sample window deliberately runs to 1800ms — past the chip's
        # settle (600ms) but well inside the blob's 6000ms loop. A
        # whole-sample-plan measurement would compute observed_coverage_ms
        # ≈ 1800ms vs expected 600ms (ratio 3.0, "too-long" correctness
        # fail); the target-span measurement must not.
        "sample_plan": {
            "target_times_ms": [0, 150, 300, 450, 600, 900, 1200, 1500, 1800],
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

    im = (r.get("assessment") or {}).get("intent_match") or {}
    dm = im.get("duration_match") or {}

    obs = dm.get("observed_coverage_ms")
    ratio = dm.get("ratio")
    verdict = dm.get("verdict")
    assert obs is not None and ratio is not None, (
        f"duration_match must be populated; got {dm}"
    )
    # The chip's onset is at t=0 and settle ~= 600. Allow a generous
    # window (300-900ms) — the sample at 600ms might catch progress
    # at 99% (still settling) and the next sample at 900ms is when
    # settle_ms is recorded.
    assert obs <= 950, (
        f"observed_coverage_ms must reflect the target's actual "
        f"motion span (~600ms), not the whole sample plan (1800ms); "
        f"got observed={obs}, ratio={ratio}, verdict={verdict}"
    )
    assert verdict == "match", (
        f"duration_match.verdict must be `match` for an expected "
        f"600ms motion observed in its true span; got {dm}"
    )
    assert im.get("passes") is True, (
        f"intent_match must pass when duration_match resolves "
        f"correctly against the target's span; got im={im}, "
        f"failed_gates={r.get('failed_gates')}"
    )
    # verdict_human must explain WHY a ratio<1.0 is still `match`
    # under the target-span gate, so agents don't have to read
    # SKILL.md to interpret it.
    vh = r.get("verdict_human") or ""
    assert ratio < 0.8, (
        f"precondition: this fixture is supposed to produce a ratio "
        f"below 0.8 to exercise the verdict_human annotation; got "
        f"ratio={ratio}"
    )
    assert "sampling noise" in vh and "target-span gate" in vh, (
        f"verdict_human must annotate the sampling-noise case when "
        f"duration_match.ratio < 0.8 with `match` verdict; got "
        f"verdict_human={vh!r}"
    )
    print(
        f"OK  duration_match measured against the chip's onset→settle "
        f"span (observed={obs:.0f}ms, ratio={ratio:.2f}, verdict={verdict}) "
        f"— ambient blob loop did not inflate the verdict; "
        f"verdict_human carries the sampling-noise note"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
