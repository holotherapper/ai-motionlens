#!/usr/bin/env python3
"""Regression guard: the capture pixel-lag harm stays resolved.

Modern headless Chrome's `Page.captureScreenshot` cannot reflect a
post-keyframe compositor commit under a paused virtual clock (no
`HeadlessExperimental.beginFrame` — a known platform limitation of
the current headless build). The raw screenshot pixels therefore lag
on sharp near-instant keyframes (`20%{left:200px} 20.1%{left:700px}`
in jank.html).

The harm — an agent being DECEIVED by stale pixels — is resolved two ways,
both asserted here:

1. The authoritative per-frame channel (`frame.layout_snapshot`, DOM bbox)
   is exact under the virtual clock: at the post-teleport sample the box
   is past the 700px jump, NOT stuck at the pre-jump position. This is the
   data `motion.contact_sheet` draws as the green DOM-truth overlay and
   the data intent_match / moved_selectors / easing / coverage already use.
2. A contact sheet is still produced (the agent-facing visual that now
   carries the DOM-truth overlay).

If this regresses (layout_snapshot itself goes stale, or the contact
sheet stops being produced) the agent could again be misled.
"""
from __future__ import annotations

import os
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    contract = {
        "url": fixture_url("jank.html"),
        "viewport": {
            "width": 1280,
            "height": 800,
            "device_scale_factor": 1.0,
            "headless": True,
        },
        "episode_intent": {
            "description": "Box moves left to right over 1000ms.",
            "expected_duration_ms": 1000,
            "expected_kinds": ["translate"],
            "expected_targets": [],
        },
        "sample_plan": {
            "target_times_ms": [0, 150, 260, 400, 1000],
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
        # (1) below reads frames[].layout_snapshot directly to prove the
        # DOM channel is exact (not stale) under the virtual clock.
        # layout_snapshot is stripped from the response by default, so opt
        # in. The contact-sheet overlay path in (2) is independent of this
        # flag (the overlay is baked server-side from the ledger).
        "include_frame_detail": True,
    }
    with McpClient() as c:
        r = c.call("motion.verify", contract)

    # --- (1) authoritative DOM channel is exact at the post-teleport sample
    by_t = {}
    for f in r["frames"]:
        ls = f.get("layout_snapshot") or {}
        for el in ls.get("elements", []) or []:
            if "box" in (el.get("selector") or ""):
                by_t[f["t_ms"]] = el["bbox"][0]

    assert 0.0 in by_t and 260.0 in by_t and 1000.0 in by_t, (
        f"missing samples; got {sorted(by_t)}"
    )
    # jank.html keyframes: 0%@0=0, 20%@200=200, 20.1%@201=700, 100%@1000=1000.
    # At t=0 the box is near the left start.
    assert by_t[0.0] < 120.0, f"t=0 box should be near start, got {by_t[0.0]}"
    # At t=260 the 700px teleport has happened — the DOM is past it, NOT
    # stuck at the pre-jump (~200) position. This is exactly where the raw
    # screenshot pixels lag; the layout_snapshot must stay exact.
    assert by_t[260.0] > 650.0, (
        f"layout_snapshot went stale at the post-teleport sample: "
        f"t=260 box_x={by_t[260.0]} (expected >650 — past the 700px jump). "
        f"The DOM-truth channel the overlay/assessment rely on regressed."
    )
    assert by_t[1000.0] > 900.0, f"t=1000 not at end: {by_t[1000.0]}"
    assert by_t[0.0] < by_t[260.0] <= by_t[1000.0], f"not monotonic: {by_t}"

    # --- (2) the agent-facing contact sheet (carrying the DOM-truth
    #         overlay) is produced and on disk.
    cs = r.get("contact_sheet")
    assert cs and cs.get("artifact_local_path"), f"no contact sheet: {cs}"
    p = cs["artifact_local_path"]
    assert os.path.exists(p) and os.path.getsize(p) > 1000, (
        f"contact sheet missing/empty: {p}"
    )

    print(
        f"OK  DOM(t,box_x)={{0:{by_t[0.0]:.0f}, 260:{by_t[260.0]:.0f}, "
        f"1000:{by_t[1000.0]:.0f}}}  contact_sheet={os.path.basename(p)}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
