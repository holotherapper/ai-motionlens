#!/usr/bin/env python3
"""recipes.scroll_animation_check end-to-end with section progress + intent diff."""
from __future__ import annotations

import os
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    with McpClient() as c:
        sid = c.call(
            "session.launch",
            {
                "url": fixture_url("scroll-section.html"),
                "viewport_width": 1280,
                "viewport_height": 720,
                "headless": True,
            },
        )["session_id"]
        try:
            c.call("episode.start", {"session_id": sid})
            c.call(
                "episode.set_intent",
                {
                    "session_id": sid,
                    "description": "Hero scales+rotates+color-shifts across the pinned section.",
                    "expected_progress_range": [0.0, 1.0],
                    "expected_kinds": ["scale", "rotate", "color-change"],
                    "forbidden_kinds": ["disappearance"],
                },
            )
            r = c.call(
                "recipes.scroll_animation_check",
                {
                    "session_id": sid,
                    "progresses_in_section": {
                        "selector": "#scrub-section",
                        "values": [0.0, 0.25, 0.5, 0.75, 1.0],
                    },
                    "target_selectors": ["#hero"],
                },
            )
            assert r["series"] is not None
            assert r["contact_sheet"] is not None
            assert r["assessment"] is not None
            cs = r["contact_sheet"]
            assert os.path.exists(cs["artifact_local_path"])
            sy = r["series"]["scroll_positions"]
            assert sy == sorted(sy), "scroll positions must be ascending"
            im = r["assessment"]["intent_match"]
            print(f"intent.passes: {im['passes']}")
            print(f"  detected: {r['assessment']['detected_motion_kinds']}")
            print(f"  expected_seen: {im['expected_kinds_seen']}")
            print(f"  expected_missing: {im['expected_kinds_missing']}")
            print(f"  scroll_range: {im['scroll_range_match']}")
            assert im["scroll_range_match"]["verdict"] == "match"
            # With the rotate-decomposition fix, all three expected kinds
            # (scale, rotate, color-change) should be detected.
            assert "rotate" not in im["expected_kinds_missing"], (
                "rotate should be detected after matrix decomposition fix"
            )
            assert im["description"] == (
                "Hero scales+rotates+color-shifts across the pinned section."
            )
        finally:
            c.call("session.close", {"session_id": sid})
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
