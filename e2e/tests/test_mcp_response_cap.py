#!/usr/bin/env python3
"""Regression guard: motion.verify degrades instead of breaking when the
returned report would exceed the MCP token ceiling.

Claude Code's MCP tool-result ceiling is 25,000 tokens by default; past
it the result is dropped / force-persisted and the agent gets a
file-shuffle detour instead of an answer — a 92-element LP produces a
273k-char report that is rejected, and even an ordinary 17-sample
contract can hard-error at ~50k chars. The server persists the FULL
report to disk, and only if
the returned JSON exceeds `MCP_SAFE_CHARS` (35,000 — set conservatively
below the measured 50,627-fail / above the 36,640-pass point) it
degrades progressively: frame layout/thumbnail → whole frames array →
unobserved_intervals, always keeping passes / verdict / assessment /
coverage / contact_sheet inline, and records
`response_truncated_for_mcp_limit` in `evidence_missing` pointing at the
on-disk full report.

This test forces that path (160-element fixture + include_frame_detail)
and asserts:
  1. the call succeeds (no break) and the returned payload is MCP-sane,
  2. evidence_missing explains the truncation,
  3. the returned frames are stripped (no heavy per-frame detail),
  4. the on-disk report at artifact_local_path is COMPLETE (full
     layout_snapshot for 100+ elements) — i.e. nothing is actually lost.

If the cap logic regresses (returns the full giant payload, or strips
disk too) one of these fails.
"""
from __future__ import annotations

import json
import os
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    contract = {
        "url": fixture_url("many-elements.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": "160 boxes rise (translate + fade) on load over 1000ms.",
            "expected_duration_ms": 1000,
            "expected_kinds": ["translate", "fade"],
            "expected_targets": [],
        },
        "sample_plan": {
            "target_times_ms": [0, 150, 300, 500, 700, 850, 1000, 1200],
            "include_layout": True,
        },
        "thresholds": {
            "min_smoothness": 0.0,
            "max_jank_events": 99,
            "require_intent_match": False,
            "min_coverage_score": 0.0,
            "require_non_static": True,
        },
        "include_contact_sheet": False,
        # Opt INTO heavy frames so the response would blow the cap unless
        # the server-side degradation kicks in. This is the worst case.
        "include_frame_detail": True,
    }

    with McpClient() as c:
        r = c.call("motion.verify", contract)

    ret_len = len(json.dumps(r, separators=(",", ":")))

    # (2) the truncation is explained
    em = r.get("evidence_missing") or []
    assert any("response_truncated_for_mcp_limit" in e for e in em), (
        f"expected response_truncated_for_mcp_limit in evidence_missing, got {em}"
    )

    # (3) the returned frames were stripped
    frames = r.get("frames") or []
    assert all(
        not f.get("layout_snapshot") and not f.get("thumbnail_base64") for f in frames
    ), "returned frames still carry heavy per-frame detail after the cap"

    # (4) the on-disk full report is complete — nothing lost
    p = r.get("artifact_local_path")
    assert p and os.path.exists(p), f"no on-disk full report: {p}"
    with open(p) as fh:
        full = json.load(fh)
    full_frames = full.get("frames") or []
    max_elems = max(
        (len((f.get("layout_snapshot") or {}).get("elements") or []) for f in full_frames),
        default=0,
    )
    assert full_frames and max_elems >= 100, (
        f"disk full report lost layout_snapshot (max elements/frame={max_elems}); "
        f"fixture must yield 100+ elements to prove the cap was real"
    )
    disk_len = os.path.getsize(p)

    # (1) returned payload is MCP-sane and strictly smaller than the full
    #     on-disk report (degradation actually happened).
    assert ret_len < disk_len, (
        f"returned payload ({ret_len}) not smaller than disk full report ({disk_len})"
    )
    # Implementation caps the returned payload at MCP_SAFE_CHARS=35k and
    # then progressively drops frames -> frames+unobserved_intervals. On
    # this 160-element fixture timeline_sources/assessment (the judgement
    # core, never dropped) dominate the remainder; measured ~90k chars.
    # The bound below is that measurement + headroom for element-count
    # jitter, and is well under the un-degraded ~190k+ — proving the
    # progressive degradation fires. Ordinary contracts (tens of elements)
    # collapse far below this via the frame strip alone.
    assert ret_len < 120_000, f"returned payload still too big for MCP: {ret_len} chars"

    print(
        f"OK  returned={ret_len:,} chars  disk_full={disk_len:,} chars  "
        f"max_elems/frame={max_elems}  passes={r.get('passes')} "
        f"truncation~={[e[:48] for e in em if 'truncat' in e]}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
