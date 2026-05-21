#!/usr/bin/env python3
"""Regression guard: jank_events carry a `kind` and the two number fields
are normalised per kind.

Two detectors feed `jank_events`:
  - pixel-cv-outlier   : delta_ratio = changed-pixel ratio [0,1]
                         (same basis as per_transition_delta);
                         z_score = (delta-mean)/stddev, >=2 outlier.
  - positional-teleport: delta_ratio = jump / viewport-width [0,1];
                         z_score = multiple of the element's median
                         per-interval move, >=4 = teleport (NOT a
                         standard score).

Without a label, both detectors write the same unlabeled
`delta_ratio`/`z_score`, so a 0.06 positional jump looks 2 orders off
the 0.0003 pixel delta for the same interval with no way to tell which
basis applied. `kind` makes the basis explicit so the agent can read
each field correctly and tell an intended big beat from a real
discontinuity.

jank.html teleports `.box` (left 200px -> 700px within 0.1% of the
timeline) so a positional-teleport must be detected and labeled, with a
median-multiple z_score (>=4), not a standard score.

It also guards the report-body self-description: the `jank-spike`
diagnosis message states its basis per `kind` (not a bare "Nσ"),
`assessment.summary` caveats that
smoothness is a within-sample-plan relative score, and `verdict_human`
inlines the advisory-only caveat (jank-events is an advisory gate, so it
must NOT claim a correctness defect) — so an agent reading the report
ALONE can't misread σ / smoothness / passes:false. The contract sets
`max_jank_events:0` so jank.html deterministically fails on an
advisory-only gate, exercising the verdict_human advisory branch.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url

VALID_KINDS = {"pixel-cv-outlier", "positional-teleport"}


def main() -> int:
    contract = {
        "url": fixture_url("jank.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": "Box translates left to right over 1000ms with a sharp jump.",
            "expected_duration_ms": 1000,
            "expected_kinds": ["translate"],
            "expected_targets": [],
        },
        "sample_plan": {
            "target_times_ms": [0, 150, 260, 400, 1000],
            "include_layout": True,  # positional detector needs layout bbox
        },
        "thresholds": {
            "min_smoothness": 0.0,
            # jank.html has 1 positional jank -> passes:false on an
            # advisory-only gate, exercising the verdict_human advisory branch.
            "max_jank_events": 0,
            "require_intent_match": False,
            "min_coverage_score": 0.0,
            "require_non_static": True,
        },
        "include_contact_sheet": False,
    }
    with McpClient() as c:
        r = c.call("motion.verify", contract)

    je = r["assessment"]["jank_events"]
    assert je, "expected jank_events on jank.html's teleport, got none"

    kinds = {e.get("kind") for e in je}
    assert None not in kinds, f"a jank_event is missing `kind`: {je}"
    assert kinds <= VALID_KINDS, f"unknown jank kind(s): {kinds - VALID_KINDS}"

    # jank.html jumps .box 200->700px in ~1ms — a positional teleport.
    assert "positional-teleport" in kinds, (
        f"jank.html teleport not classified as positional-teleport; kinds={kinds}"
    )

    # Per-kind field-basis sanity (the whole point of the change).
    for e in je:
        assert 0.0 <= e["delta_ratio"] <= 1.0, f"delta_ratio out of [0,1]: {e}"
        if e["kind"] == "positional-teleport":
            assert e["z_score"] >= 4.0, (
                f"positional-teleport z_score must be a median-multiple >=4, got {e}"
            )
        elif e["kind"] == "pixel-cv-outlier":
            assert e["z_score"] >= 2.0, (
                f"pixel-cv-outlier z_score must be a standard score >=2, got {e}"
            )

    # --- report-body self-description ---
    jhints = [h for h in (r.get("diagnosis_hints") or []) if h.get("code") == "jank-spike"]
    assert jhints, "no jank-spike diagnosis hint emitted"
    pj_hint = next(
        (h for h in jhints if (h.get("observed") or {}).get("kind") == "positional-teleport"),
        None,
    )
    assert pj_hint, (
        f"no positional-teleport jank-spike hint; observed={[h.get('observed') for h in jhints]}"
    )
    msg = pj_hint["message"]
    assert "positional-teleport" in msg and "NOT a σ" in msg, (
        f"positional jank message must self-explain its basis, not a bare σ: {msg!r}"
    )

    summary = r["assessment"].get("summary") or ""
    assert (
        "relative WITHIN this sample_plan" in summary
        or "not an absolute quality score" in summary
    ), f"summary must caveat smoothness sampling-dependence: {summary!r}"

    assert r["passes"] is False, "contract sets max_jank_events:0 so jank.html must fail"
    vh = r.get("verdict_human") or ""
    # jank-events is an ADVISORY gate, so verdict_human must inline the
    # advisory-only caveat — and must NOT claim a correctness defect.
    assert "ADVISORY gates only" in vh and "passes:true is reliable" in vh, (
        f"advisory-only failure must inline the advisory caveat: {vh!r}"
    )
    assert "CORRECTNESS gate" not in vh, (
        f"jank-events is advisory; verdict_human must not assert a correctness defect: {vh!r}"
    )

    sample = next(e for e in je if e["kind"] == "positional-teleport")
    print(
        f"OK  kinds={sorted(kinds)}  n={len(je)}  "
        f"positional sample: delta_ratio={sample['delta_ratio']:.4f} "
        f"z_score(x median)={sample['z_score']:.1f}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
