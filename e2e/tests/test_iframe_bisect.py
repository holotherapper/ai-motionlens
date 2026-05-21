#!/usr/bin/env python3
"""iframe-targeted triggers and scratch-page replay via frame.bisect."""
from __future__ import annotations

import pathlib
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def wait_for_iframe_ready(c: McpClient, sid: str, name: str, attempts: int = 40) -> None:
    """Spin until the named iframe's contentDocument has fully loaded.
    The host page returns from goto() as soon as it parses, but its child
    iframe is still fetching; without this wait the first iframe-targeted
    trigger races the load.
    """
    js = (
        "(function(){"
        "var f = document.querySelector('iframe[name=\"" + name + "\"]');"
        "return !!(f && f.contentDocument && f.contentDocument.readyState === 'complete'"
        " && f.contentDocument.getElementById('inner-btn'));"
        "})()"
    )
    for _ in range(attempts):
        if c.call("frame.dom_query", {"session_id": sid, "js": js})["value"]:
            return
        time.sleep(0.05)
    raise RuntimeError(f"iframe '{name}' did not become ready in time")


def main() -> int:
    with McpClient() as c:
        sid = c.call(
            "session.launch",
            {"url": fixture_url("iframe-host.html"), "headless": True},
        )["session_id"]
        try:
            wait_for_iframe_ready(c, sid, "child")
            ep = c.call("episode.start", {"session_id": sid})
            eid = ep["episode_id"]

            # Trigger inside the named iframe.
            c.call(
                "trigger.click",
                {
                    "session_id": sid,
                    "selector": "#inner-btn",
                    "frame_path": ["child"],
                },
            )
            # Read state inside the iframe via trigger.evaluate (iframe-aware).
            c.call(
                "trigger.evaluate",
                {
                    "session_id": sid,
                    "js": "window.__inner_seen = document.getElementById('flag').textContent",
                    "frame_path": ["child"],
                },
            )

            # Advance and capture a few frames on the live clock.
            series = c.call(
                "frame.capture_series",
                {"session_id": sid, "target_times_ms": [0, 50, 100, 200]},
            )
            assert len(series["frames"]) == 4

            # Now bisect a backwards interval. This forces scratch-page replay.
            bisect = c.call(
                "frame.bisect",
                {
                    "session_id": sid,
                    "episode_id": eid,
                    "interval": {"t0_ms": 50, "t1_ms": 100},
                },
            )
            assert bisect["replay"] == "scratch", bisect
            # The replayed frame is now part of the ledger.
            ledger = c.call("evidence.list", {"session_id": sid, "episode_id": eid})
            assert any(f["frame_id"] == bisect["frame"]["frame_id"] for f in ledger["frames"]), (
                "bisect frame must be appended to ledger.frames"
            )

            # episode.replay_to also returns frame_id and persists it.
            replay = c.call(
                "episode.replay_to",
                {"session_id": sid, "episode_id": eid, "t_ms": 75},
            )
            assert replay["frame_id"], replay
            ledger2 = c.call("evidence.list", {"session_id": sid, "episode_id": eid})
            assert any(f["frame_id"] == replay["frame_id"] for f in ledger2["frames"]), (
                "replay_to frame must be appended to ledger.frames"
            )
            print(
                f"iframe + scratch-replay: bisect at {bisect['frame']['t_ms']:.0f}ms, "
                f"replay_to frame {replay['frame_id'][:12]}..."
            )
        finally:
            c.call("session.close", {"session_id": sid})
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
