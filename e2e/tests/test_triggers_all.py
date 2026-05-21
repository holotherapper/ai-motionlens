#!/usr/bin/env python3
"""Every trigger.* variant must (a) dispatch correctly, (b) record an
`at_t_ms` of the current virtual clock (NOT some implicit 50ms ahead), and
(c) leave the virtual clock paused at the same value it was before the call.

This guards the "Trigger no longer burns 50ms of virtual time" invariant.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    with McpClient() as c:
        sid = c.call(
            "session.launch",
            {"url": fixture_url("interactive.html"), "headless": True},
        )["session_id"]
        try:
            ep = c.call("episode.start", {"session_id": sid})
            eid = ep["episode_id"]

            def now() -> float:
                return c.call("clock.status", {"session_id": sid})["virtual_now_ms"]

            before = now()
            c.call("trigger.click", {"session_id": sid, "selector": "#counter-btn"})
            after = now()
            assert before == after, (
                f"trigger.click advanced virtual clock {before} -> {after}; "
                "should be paused (no implicit budget burn)"
            )

            c.call("trigger.hover", {"session_id": sid, "selector": "#hover-target"})
            assert now() == before

            c.call(
                "trigger.type",
                {"session_id": sid, "selector": "#name-input", "text": "hello"},
            )
            assert now() == before

            c.call("trigger.scroll", {"session_id": sid, "x": 0, "y": 200})
            assert now() == before

            c.call(
                "trigger.evaluate",
                {
                    "session_id": sid,
                    "js": "document.getElementById('evaluated').textContent = 'ran'",
                },
            )
            assert now() == before

            # Each trigger should be recorded in the ledger at the same t_ms.
            ledger = c.call("evidence.list", {"session_id": sid, "episode_id": eid})
            triggers = ledger["triggers"]
            assert len(triggers) == 5, triggers
            for t in triggers:
                assert t["at_t_ms"] == before, t

            # And the side effects took: clock is still paused, but the DOM
            # changed synchronously.
            counter = c.call(
                "frame.dom_query",
                {"session_id": sid, "js": "window.__count"},
            )["value"]
            assert counter == 1, counter
            evaluated = c.call(
                "frame.dom_query",
                {
                    "session_id": sid,
                    "js": "document.getElementById('evaluated').textContent",
                },
            )["value"]
            assert evaluated == "ran", evaluated
            print(f"triggers all: 5 dispatched at t={before}ms, all side effects took")
        finally:
            c.call("session.close", {"session_id": sid})
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
