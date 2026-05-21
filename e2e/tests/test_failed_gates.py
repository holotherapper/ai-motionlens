#!/usr/bin/env python3
"""Regression guard: passes:false is attributable, with reliability.

`passes` ANDs five gates of unequal trust — the most prominent field
is the least trustworthy on its own. The report exposes
`failed_gates[]`, each with `reliability`:
  - correctness : intent-match / non-static — a real defect.
  - advisory    : smoothness / jank-events / coverage — known to false-
                  negative on legitimate work (a full-screen beat, a
                  scaleX bar, rAF/GSAP coverage 0, a count→burst loader).

So an agent / CI can tell "passes:false because the declared intent was
not met" (act on it) from "passes:false only because an advisory metric
tripped on a deliberate big beat" (triage, don't block).

Two cases:
  A. advisory-only — jank.html with max_jank_events:0. jank.html's
     teleport is 1 positional jank; nothing else fails. Every
     failed_gate must be `advisory`.
  B. correctness  — css-animation.html with a bogus expected_target and
     require_intent_match. intent_match cannot pass, so `intent-match`
     fails with reliability `correctness`.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    vp = {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True}

    # --- Case A: advisory-only failure -------------------------------
    contract_a = {
        "url": fixture_url("jank.html"),
        "viewport": vp,
        "episode_intent": {
            "description": "Box moves with a sharp jump.",
            "expected_duration_ms": 1000,
            "expected_kinds": ["translate"],
            "expected_targets": [],
        },
        "sample_plan": {"target_times_ms": [0, 150, 260, 400, 1000], "include_layout": True},
        "thresholds": {
            "min_smoothness": 0.0,
            "max_jank_events": 0,          # jank.html has 1 positional jank
            "require_intent_match": False,
            "min_coverage_score": 0.0,
            "require_non_static": True,
        },
        "include_contact_sheet": False,
    }
    with McpClient() as c:
        ra = c.call("motion.verify", contract_a)

    assert ra["passes"] is False, "Case A: max_jank_events:0 must fail jank.html"
    fga = ra.get("failed_gates") or []
    assert fga, f"Case A: failed_gates empty despite passes:false: {ra.get('verdict_human')}"
    assert all(g["gate"] == "jank-events" for g in fga), (
        f"Case A: only the jank-events gate should fail, got {[g['gate'] for g in fga]}"
    )
    assert all(g["reliability"] == "advisory" for g in fga), (
        f"Case A: jank-events must be reliability=advisory, got {fga}"
    )
    assert all(g.get("detail") for g in fga), f"Case A: each failed_gate needs a detail: {fga}"

    # --- Case B: correctness failure ---------------------------------
    contract_b = {
        "url": fixture_url("css-animation.html"),
        "viewport": vp,
        "episode_intent": {
            "description": "A selector that does not exist is expected to move.",
            "expected_duration_ms": 1000,
            "expected_kinds": ["translate"],
            "expected_targets": [".this-selector-does-not-exist"],
        },
        "sample_plan": {"target_times_ms": [0, 250, 500, 1000], "include_layout": True},
        "thresholds": {
            "min_smoothness": 0.0,
            "max_jank_events": 99,
            "require_intent_match": True,   # forces the intent-match gate
            "min_coverage_score": 0.0,
            "require_non_static": True,
        },
        "include_contact_sheet": False,
    }
    with McpClient() as c:
        rb = c.call("motion.verify", contract_b)

    assert rb["passes"] is False, "Case B: bogus expected_target must fail intent-match"
    fgb = rb.get("failed_gates") or []
    by_gate = {g["gate"]: g for g in fgb}
    assert "intent-match" in by_gate, (
        f"Case B: intent-match gate must be present, got {list(by_gate)}"
    )
    assert by_gate["intent-match"]["reliability"] == "correctness", (
        f"Case B: intent-match must be reliability=correctness, got {by_gate['intent-match']}"
    )
    assert by_gate["intent-match"].get("detail"), "Case B: intent-match needs a detail"

    # The whole point: an agent can separate the two without guessing.
    a_all_advisory = all(g["reliability"] == "advisory" for g in fga)
    b_has_correctness = any(g["reliability"] == "correctness" for g in fgb)
    assert a_all_advisory and b_has_correctness, (
        "advisory-only vs correctness must be distinguishable from failed_gates alone"
    )

    # verdict_human must speak PER reliability — a blanket triage hedge
    # would dilute the one trustworthy correctness signal.
    vha = ra.get("verdict_human") or ""
    assert "ADVISORY gates only" in vha and "CORRECTNESS gate" not in vha, (
        f"Case A (advisory-only) verdict_human must hedge, not claim a defect: {vha!r}"
    )
    vhb = rb.get("verdict_human") or ""
    assert "CORRECTNESS gate" in vhb and "real defect" in vhb, (
        f"Case B (correctness) verdict_human must state a real defect plainly: {vhb!r}"
    )

    print(
        f"OK  A: {[(g['gate'], g['reliability']) for g in fga]}  "
        f"B: {[(g['gate'], g['reliability']) for g in fgb]}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
