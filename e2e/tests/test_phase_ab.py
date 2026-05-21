#!/usr/bin/env python3
"""Phase A (typed errors / capabilities honesty / dom_query / format / local_path)
+ Phase B (thumbnail off / layout filters / anomaly suppression) verification."""
from __future__ import annotations

import base64
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
                "url": fixture_url("broken-layout.html"),
                "viewport_width": 1280,
                "viewport_height": 720,
                "headless": True,
            },
        )["session_id"]
        try:
            caps = c.call("session.capabilities", {"session_id": sid})
            assert caps["live_clock_forward_only"] is True

            c.call("episode.start", {"session_id": sid})

            q1 = c.call(
                "frame.dom_query",
                {"session_id": sid, "js": "document.querySelectorAll('.keep').length"},
            )
            assert q1["value"] == 2

            f_png = c.call(
                "frame.capture", {"session_id": sid, "format": "png", "thumbnail": True}
            )
            f_jpg = c.call(
                "frame.capture", {"session_id": sid, "format": "jpeg", "thumbnail": True}
            )
            png_head = base64.b64decode(f_png["thumbnail_base64"][:24])[:4]
            jpg_head = base64.b64decode(f_jpg["thumbnail_base64"][:24])[:3]
            assert png_head == bytes.fromhex("89504e47")
            assert jpg_head == bytes.fromhex("ffd8ff")
            assert os.path.exists(f_png["artifact_local_path"])

            f_default = c.call("frame.capture", {"session_id": sid})
            assert f_default.get("thumbnail_base64") is None

            unfiltered = c.call("frame.layout_probe", {"session_id": sid})
            filt = c.call(
                "frame.layout_probe", {"session_id": sid, "selectors": [".keep"]}
            )
            assert filt["elements_returned"] == 2

            mx = c.call("frame.layout_probe", {"session_id": sid, "max_elements": 2})
            assert mx["truncated"] is True

            before = sum(
                1
                for a in unfiltered["anomalies"]
                if a["kind"] in ("content-clipped", "text-ellipsis-truncated")
            )
            silenced = c.call(
                "frame.layout_probe",
                {"session_id": sid, "ignore_selectors": [".silence-me"]},
            )
            after = sum(
                1
                for a in silenced["anomalies"]
                if a["kind"] in ("content-clipped", "text-ellipsis-truncated")
            )
            assert after < before
            real_bug = [
                a
                for a in silenced["anomalies"]
                if a["kind"] == "horizontal-viewport-overflow"
            ]
            assert len(real_bug) == 1
        finally:
            c.call("session.close", {"session_id": sid})
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
