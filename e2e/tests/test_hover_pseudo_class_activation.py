#!/usr/bin/env python3
"""Regression guard: `trigger.hover input_mode: cdp` activates the
`:hover` pseudo-class on the target element, AND the computed style
reflects the `:hover` declaration. A bare `Input.dispatchMouseEvent
{ mouseMoved }` parks the compositor cursor over the element but does
not reliably make the style engine re-apply `:hover` declarations, so
the CDP hover path pairs the mouse move with a `CSS.forcePseudoState`
call to guarantee the style change flows through.

Note: CSS *transitions* triggered by the `:hover` change have a
separate timing limitation (their `currentTime` doesn't tick under a
paused virtual clock — see SKILL.md). This test focuses on the
pseudo-class activation itself and uses a static (non-transitioned)
hover style.
"""
from __future__ import annotations

import json
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    with McpClient() as c:
        sid = c.call(
            "session.launch",
            {
                "url": fixture_url("hover-pseudo-class.html"),
                "viewport_width": 1280, "viewport_height": 800, "headless": True,
            },
        )["session_id"]
        try:
            c.call("episode.start", {"session_id": sid})

            # Sanity: before the hover trigger, neither pseudo class
            # nor styled bg are active.
            before = c.call(
                "frame.dom_query",
                {
                    "session_id": sid,
                    "js": "JSON.stringify({hover: document.getElementById('btn').matches(':hover'), bg: getComputedStyle(document.getElementById('btn')).backgroundColor})",
                },
            )
            before_v = json.loads(before.get("value") or "{}")
            assert before_v.get("hover") is False, (
                f"precondition: before the hover trigger, :hover must "
                f"not match; got {before_v}"
            )

            # The actual trigger. CDP mode is the only path that
            # touches `:hover`; JS-mode dispatch doesn't.
            c.call(
                "trigger.hover",
                {"session_id": sid, "selector": "#btn", "input_mode": "cdp"},
            )

            after = c.call(
                "frame.dom_query",
                {
                    "session_id": sid,
                    "js": "JSON.stringify({hover: document.getElementById('btn').matches(':hover'), bg: getComputedStyle(document.getElementById('btn')).backgroundColor})",
                },
            )
            after_v = json.loads(after.get("value") or "{}")
            assert after_v.get("hover") is True, (
                f"`:hover` pseudo-class must be active after CDP "
                f"hover trigger; got {after_v}"
            )
            # The fixture's `:hover` declaration sets bg to #1f6feb =
            # rgb(31, 111, 235). With `CSS.forcePseudoState` the style
            # engine applies the hovered declaration immediately, so
            # the static (non-transitioned) hover background takes
            # effect right away.
            assert "31, 111, 235" in (after_v.get("bg") or ""), (
                f"computed background must reflect the :hover "
                f"declaration; got {after_v}"
            )
            print(
                f"OK  CDP trigger.hover activates :hover and the "
                f"hovered style takes effect in computed style "
                f"({after_v})"
            )
        finally:
            c.call("session.close", {"session_id": sid})

    return 0


if __name__ == "__main__":
    sys.exit(main())
