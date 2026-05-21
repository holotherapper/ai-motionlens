#!/usr/bin/env python3
"""motion.assess distinguishes smooth vs janky animation."""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def assess_fixture(c: McpClient, url: str) -> dict:
    sid = c.call(
        "session.launch",
        {
            "url": url,
            "viewport_width": 1280,
            "viewport_height": 800,
            "headless": True,
        },
    )["session_id"]
    try:
        c.call("episode.start", {"session_id": sid})
        series = c.call(
            "frame.capture_series",
            {
                "session_id": sid,
                "target_times_ms": [0, 100, 200, 250, 300, 400, 500, 700, 1000],
            },
        )
        fids = [f["frame_id"] for f in series["frames"]]
        return c.call("motion.assess", {"session_id": sid, "frame_ids": fids})
    finally:
        c.call("session.close", {"session_id": sid})


def main() -> int:
    with McpClient() as c:
        smooth = assess_fixture(c, fixture_url("css-animation.html"))
        janky = assess_fixture(c, fixture_url("jank.html"))
        print(
            f"smooth: smoothness={smooth['smoothness']:.3f} ({smooth['smoothness_verdict']}) "
            f"jank={len(smooth['jank_events'])}"
        )
        print(
            f"janky:  smoothness={janky['smoothness']:.3f} ({janky['smoothness_verdict']}) "
            f"jank={len(janky['jank_events'])}"
        )
        assert smooth["smoothness"] > janky["smoothness"], (
            f"smooth should score higher than janky "
            f"({smooth['smoothness']:.3f} vs {janky['smoothness']:.3f})"
        )
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
