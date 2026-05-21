#!/usr/bin/env python3
"""End-to-end smoke test: every tool exercised in one session."""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    with McpClient() as c:
        tools = c.tools_list()
        assert len(tools) == 33, f"expected exactly 33 tools, got {len(tools)}"
        print(f"tools/list: {len(tools)} tools")

        sid = c.call(
            "session.launch",
            {
                "url": fixture_url("modal.html"),
                "viewport_width": 1280,
                "viewport_height": 800,
                "headless": True,
            },
        )["session_id"]
        try:
            caps = c.call("session.capabilities", {"session_id": sid})
            print(f"  capabilities: driver={caps['driver']} can_seek_back={caps['can_seek_back']}")
            assert caps["live_clock_forward_only"] is True

            ep = c.call("episode.start", {"session_id": sid})
            eid = ep["episode_id"]

            c.call(
                "episode.set_intent",
                {
                    "session_id": sid,
                    "description": "Modal fades and slides in over ~300ms.",
                    "expected_duration_ms": 300,
                    "expected_kinds": ["fade", "translate"],
                    "forbidden_kinds": ["disappearance"],
                },
            )

            tr = c.call("trigger.click", {"session_id": sid, "selector": "#open-modal"})
            print(f"  trigger.click: {tr['trigger_id'][:16]}...")

            # trigger.click dispatches at the current virtual time without
            # burning a budget, so capture starts from t=0 (relative to the
            # episode's t=0 baseline).
            series = c.call(
                "frame.capture_series",
                {
                    "session_id": sid,
                    "target_times_ms": [0, 60, 150, 240, 320, 500],
                    "layout": {"selectors": ["#modal", "#backdrop"]},
                },
            )
            fids = [f["frame_id"] for f in series["frames"]]
            print(f"  capture_series: {len(fids)} frames, {len(series['unobserved_intervals'])} gaps")

            sheet = c.call(
                "motion.contact_sheet",
                {"session_id": sid, "frame_ids": fids, "thumb_width": 256},
            )
            assert sheet["columns"] == len(fids)
            assert len(sheet["cells"]) == len(fids)

            assess = c.call("motion.assess", {"session_id": sid, "frame_ids": fids})
            print(
                f"  motion.assess: smoothness={assess['smoothness']:.3f} ({assess['smoothness_verdict']}) "
                f"jank={len(assess['jank_events'])} intent.passes={assess['intent_match']['passes']}"
            )

            bisect = c.call(
                "frame.bisect",
                {"session_id": sid, "episode_id": eid, "interval": {"t0_ms": 100, "t1_ms": 300}},
            )
            print(f"  frame.bisect: replay={bisect['replay']} cost={bisect['replay_cost_ms']:.1f}ms")

            probe = c.call("frame.layout_probe", {"session_id": sid})
            print(f"  layout_probe: {probe['element_count']} elements, {len(probe['anomalies'])} anomalies")

            q = c.call(
                "frame.dom_query",
                {"session_id": sid, "js": "document.querySelectorAll('.modal').length"},
            )
            assert isinstance(q["value"], (int, float))

            obs = c.call(
                "evidence.append_observation",
                {
                    "session_id": sid,
                    "episode_id": eid,
                    "interval": {"t0_ms": 0, "t1_ms": 320},
                    "claim": "Modal fades in monotonically.",
                    "confidence": 0.85,
                    "evidence_frame_ids": fids,
                },
            )
            assert "observation_id" in obs

            ledger = c.call("evidence.list", {"session_id": sid, "episode_id": eid})
            assert len(ledger["observations"]) >= 1
            assert ledger["intent"] is not None
        finally:
            c.call("session.close", {"session_id": sid})
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
