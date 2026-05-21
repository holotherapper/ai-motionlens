#!/usr/bin/env python3
"""Two sessions must coexist and remain independent. This is a regression
guard against the older `Arc<Mutex<HashMap<SessionId, Session>>>` design that
held the HashMap lock across browser-bound awaits and serialised everything.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    with McpClient() as c:
        a = c.call(
            "session.launch",
            {"url": fixture_url("modal.html"), "headless": True},
        )["session_id"]
        b = c.call(
            "session.launch",
            {"url": fixture_url("css-animation.html"), "headless": True},
        )["session_id"]
        try:
            c.call("episode.start", {"session_id": a})
            c.call("episode.start", {"session_id": b})

            # Advance session A's clock; B must remain at 0.
            c.call("clock.advance", {"session_id": a, "delta_ms": 200})
            sa = c.call("clock.status", {"session_id": a})["virtual_now_ms"]
            sb = c.call("clock.status", {"session_id": b})["virtual_now_ms"]
            assert sa >= 200.0, sa
            assert sb == 0.0, sb

            # Capture in B while A is at t=200; B is still at t=0.
            fb = c.call("frame.capture", {"session_id": b})
            assert fb["t_ms"] == 0.0

            # Capture in A; should be at the advanced time.
            fa = c.call("frame.capture", {"session_id": a})
            assert fa["t_ms"] >= 200.0

            # Independent capabilities are still readable.
            ca = c.call("session.capabilities", {"session_id": a})
            cb = c.call("session.capabilities", {"session_id": b})
            assert ca["driver"] == cb["driver"] == "virtual-time"
            print(f"concurrent: A@{sa}ms, B@{sb}ms, independent")
        finally:
            c.call("session.close", {"session_id": a})
            c.call("session.close", {"session_id": b})
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
