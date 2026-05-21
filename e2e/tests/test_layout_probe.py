#!/usr/bin/env python3
"""frame.layout_probe detects all six anomaly kinds on the broken-layout fixture."""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


EXPECTED_KINDS = {
    "horizontal-viewport-overflow",
    "content-clipped",
    "text-ellipsis-truncated",
    "off-screen",
    "overlap-stacking",
}


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
            snap = c.call("frame.layout_probe", {"session_id": sid})
            kinds = {a["kind"] for a in snap["anomalies"]}
            print(f"anomaly kinds detected: {sorted(kinds)}")
            missing = EXPECTED_KINDS - kinds
            assert not missing, f"missing anomaly kinds: {missing}"
        finally:
            c.call("session.close", {"session_id": sid})
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
