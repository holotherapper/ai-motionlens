#!/usr/bin/env python3
"""Regression guard: `coverage_score` is honest for a rAF / canvas
page (returns 0.0, not a spurious 1.0), and the `coverage` gate is
auto-skipped instead of force-failing the run.

raf-canvas.html moves pixels via a requestAnimationFrame canvas loop
with NO CSS animation / transition / WAAPI source. The coverage model
is defined over CSS/transition/WAAPI sources only, so for this page
the coverable set is empty and coverage is structurally unmeasurable.

Two regressions this test pins:
  (a) `coverage_score` must NOT report a confident `1.0` — a spurious
      1.0 would mark a JS-driven page "fully covered" and let gates
      pass without sampling verification.
  (b) `coverage` gate must NOT fire just because the page is rAF-driven
      — without auto-skip, agents would have to manually set
      `min_coverage_score: 0` on every JS-driven page. Auto-skip the
      gate, surface the reason in `evidence_missing`, and keep the
      other gates (intent-match / non-static / smoothness) in force.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    base = {
        "url": fixture_url("raf-canvas.html"),
        "viewport": {
            "width": 1280,
            "height": 800,
            "device_scale_factor": 1.0,
            "headless": True,
        },
        "sample_plan": {
            "target_times_ms": [0, 200, 400, 600, 800],
            "include_layout": True,
        },
        "include_contact_sheet": False,
    }

    with McpClient() as c:
        # Coverage required (min_coverage_score 0.6): coverage_score
        # must still report 0.0 honestly (NOT spuriously 1.0), but
        # the `coverage` gate must be auto-skipped because the page
        # is unassessable — and `evidence_missing` must explain why
        # so the agent can read the situation without searching.
        gated = dict(base)
        gated["thresholds"] = {
            "min_smoothness": 0.0,
            "max_jank_events": 99,
            "require_intent_match": False,
            "min_coverage_score": 0.6,
            "require_non_static": True,
        }
        r = c.call("motion.verify", gated)
        cov = r["coverage_score"]
        em = r["evidence_missing"]
        fg = r.get("failed_gates") or []
        assert cov == 0.0, (
            f"coverage_score must be 0.0 (not assessable) for a no-CSS-source "
            f"moving page, got {cov} — the false 1.0 has regressed"
        )
        assert any(
            "motion_present_but_no_coverable_sources" in s
            or "rAF" in s
            or "raf" in s.lower()
            for s in em
        ), (
            f"expected the coverage-not-assessable evidence entry, got {em}"
        )
        coverage_gates = [g for g in fg if g.get("gate") == "coverage"]
        assert not coverage_gates, (
            f"the coverage gate must be auto-skipped on a rAF/canvas page; "
            f"got {coverage_gates}"
        )
        print(
            f"  gated: passes={r['passes']} coverage={cov} "
            f"(coverage gate auto-skipped, evidence_missing carries the reason)"
        )

        # Coverage NOT required (min_coverage_score 0.0): the page is still
        # legitimately verifiable via smoothness — coverage being 0.0 must
        # not, by itself, fail it.
        ungated = dict(base)
        ungated["thresholds"] = {
            "min_smoothness": 0.0,
            "max_jank_events": 99,
            "require_intent_match": False,
            "min_coverage_score": 0.0,
            "require_non_static": True,
        }
        r2 = c.call("motion.verify", ungated)
        assert r2["assessment"]["is_static"] is False, (
            f"canvas rAF motion must register as non-static: {r2['assessment']}"
        )
        # coverage_score == 0.0 must not be the thing that fails it when
        # coverage is not required.
        assert r2["coverage_score"] == 0.0, r2["coverage_score"]
        assert r2["passes"] is True, (
            f"with min_coverage_score 0.0 the canvas page should pass on "
            f"smoothness; passes={r2['passes']} "
            f"verdict_human={r2.get('verdict_human')} em={r2['evidence_missing']}"
        )
        print(f"  ungated: passes={r2['passes']} (verifiable via smoothness)")
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
