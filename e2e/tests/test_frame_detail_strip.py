#!/usr/bin/env python3
"""Regression guard: `include_frame_detail` strips frames[] without
changing the verdict or any score.

`motion.verify` returns `frames[]` as the single largest element of the
response (measured 48-56% of the payload), almost all of it per-frame
`layout_snapshot`. That snapshot is the *input* to `assessment`,
`coverage_score`, the `contact_sheet` DOM-truth overlay and
`diagnosis_hints` — all computed server-side BEFORE the response is
built. So the response can drop `layout_snapshot` / `thumbnail_base64`
from `frames[]` (default) without moving the pass/fail bar.

This test asserts the three properties that make the optimisation safe:

  (1) default (no `include_frame_detail`)  -> every frame is image-only
      (no `layout_snapshot`, no `thumbnail_base64`).
  (2) `include_frame_detail: true`         -> layout_snapshot is back.
  (3) the agent-facing verdict surface is byte-identical between the two
      runs: passes / verdict / coverage_score, the deterministic parts of
      assessment (is_static / moved_selectors / detected_motion_kinds /
      intent_match), the diagnosis_hints code list, and the contact sheet
      (produced in both, same per-cell t_ms). pixel-derived continuous
      values (smoothness float, jank z-scores) are NOT asserted equal —
      they carry Chrome's known screenshot non-determinism and are
      orthogonal to the strip; `passes` already folds in the threshold
      decisions that matter.

If a future change makes the strip happen before a consumer reads the
layout (or drops a field the agent needs to decide pass/fail), (3)
fails.
"""
from __future__ import annotations

import json
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def base_contract() -> dict:
    return {
        "url": fixture_url("css-animation.html"),
        "viewport": {
            "width": 1280,
            "height": 800,
            "device_scale_factor": 1.0,
            "headless": True,
        },
        "episode_intent": {
            "description": "Box translates left 0->1000px over 1000ms linear, on load.",
            "expected_duration_ms": 1000,
            "expected_kinds": ["translate"],
            "expected_targets": [],
        },
        "sample_plan": {
            "target_times_ms": [0, 250, 500, 750, 1000],
            "include_layout": True,
        },
        "thresholds": {
            "min_smoothness": 0.6,
            "max_jank_events": 99,
            "require_intent_match": False,
            "min_coverage_score": 0.0,
            "require_non_static": True,
        },
        "include_contact_sheet": True,
    }


def main() -> int:
    strip_contract = base_contract()  # include_frame_detail omitted -> default false
    full_contract = base_contract()
    full_contract["include_frame_detail"] = True

    with McpClient() as c:
        r_strip = c.call("motion.verify", strip_contract)
    with McpClient() as c:
        r_full = c.call("motion.verify", full_contract)

    strip_bytes = len(json.dumps(r_strip, separators=(",", ":")))
    full_bytes = len(json.dumps(r_full, separators=(",", ":")))

    # (1) default: every frame is image-only.
    assert r_strip["frames"], "no frames captured"
    for f in r_strip["frames"]:
        assert not f.get("layout_snapshot"), (
            f"default response still carries layout_snapshot at t={f['t_ms']}"
        )
        assert not f.get("thumbnail_base64"), (
            f"default response still carries thumbnail_base64 at t={f['t_ms']}"
        )

    # (2) opt-in restores the per-frame DOM snapshot.
    assert any(
        (f.get("layout_snapshot") or {}).get("elements") for f in r_full["frames"]
    ), "include_frame_detail:true did not restore layout_snapshot"

    # (3) the verdict surface the agent reads is identical.
    assert r_strip["passes"] == r_full["passes"], "passes diverged"
    assert r_strip["verdict"] == r_full["verdict"], "verdict diverged"
    assert r_strip["coverage_score"] == r_full["coverage_score"], "coverage_score diverged"

    a1, a2 = r_strip["assessment"], r_full["assessment"]
    for k in ("is_static", "moved_selectors", "detected_motion_kinds", "intent_match"):
        assert a1.get(k) == a2.get(k), f"assessment.{k} diverged: {a1.get(k)} vs {a2.get(k)}"

    h1 = [h["code"] for h in r_strip.get("diagnosis_hints", [])]
    h2 = [h["code"] for h in r_full.get("diagnosis_hints", [])]
    assert h1 == h2, f"diagnosis_hints codes diverged: {h1} vs {h2}"

    cs1, cs2 = r_strip.get("contact_sheet"), r_full.get("contact_sheet")
    assert (cs1 is None) == (cs2 is None), "contact_sheet presence diverged"
    if cs1:
        t1 = [round(c["t_ms"], 3) for c in cs1["cells"]]
        t2 = [round(c["t_ms"], 3) for c in cs2["cells"]]
        assert t1 == t2, f"contact_sheet cell t_ms diverged: {t1} vs {t2}"

    # The strip must actually shrink the response (the whole point).
    assert strip_bytes < full_bytes, (
        f"strip did not shrink the payload: strip={strip_bytes}B full={full_bytes}B"
    )
    reduction = 100.0 * (full_bytes - strip_bytes) / full_bytes
    print(
        f"OK  verdict-surface identical; payload {full_bytes}B -> {strip_bytes}B "
        f"(-{reduction:.0f}%)  passes={r_strip['passes']} verdict={r_strip['verdict']}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
