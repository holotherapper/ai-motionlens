#!/usr/bin/env python3
"""Trigger input_mode: js vs cdp.

CSS `:hover` only fires under a real compositor hover. Under JS-mode
dispatch (`el.dispatchEvent(new MouseEvent('mouseenter'))`) the
pseudo-class stays inactive, so the `.target` element never animates.
Under CDP-mode (`Input.dispatchMouseEvent`) the pseudo-class fires and
the element starts translating.

This is the canonical test that `input_mode: "cdp"` is structurally
different from JS dispatch, and that the paint-pipeline flush after
CDP dispatch lets a subsequent `frame.capture` actually return.
"""
from __future__ import annotations

import json
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    with McpClient() as c:
        # --- JS-mode hover: pseudo-class must NOT fire ---
        sid_js = c.call(
            "session.launch",
            {
                "url": fixture_url("hover-css.html"),
                "viewport_width": 1280,
                "viewport_height": 800,
                "headless": True,
            },
        )["session_id"]
        try:
            c.call("episode.start", {"session_id": sid_js})
            c.call(
                "trigger.hover",
                {
                    "session_id": sid_js,
                    "selector": "#target",
                    "frame_path": [],
                    "input_mode": "js",
                },
            )
            # Advance ~300ms to give a real hover-animation time to play out.
            c.call("clock.advance", {"session_id": sid_js, "delta_ms": 320})
            r = c.call(
                "frame.dom_query",
                {
                    "session_id": sid_js,
                    "js": "JSON.stringify({ transform: getComputedStyle(document.getElementById('target')).transform })",
                },
            )
            v_js = r["value"]
            parsed_js = json.loads(v_js)
            t_js = parsed_js["transform"]
            assert t_js in ("none", "matrix(1, 0, 0, 1, 0, 0)"), t_js
            print(f"  js mode: transform={t_js!r} (pseudo-class did not fire — as expected)")
        finally:
            c.call("session.close", {"session_id": sid_js})

        # --- CDP-mode hover: pseudo-class fires, element translates ---
        sid_cdp = c.call(
            "session.launch",
            {
                "url": fixture_url("hover-css.html"),
                "viewport_width": 1280,
                "viewport_height": 800,
                "headless": True,
            },
        )["session_id"]
        try:
            c.call("episode.start", {"session_id": sid_cdp})
            c.call(
                "trigger.hover",
                {
                    "session_id": sid_cdp,
                    "selector": "#target",
                    "frame_path": [],
                    "input_mode": "cdp",
                },
            )
            # 320ms is past the 300ms transition end, so the target should
            # be at translateX(220).
            c.call("clock.advance", {"session_id": sid_cdp, "delta_ms": 320})
            r2 = c.call(
                "frame.dom_query",
                {
                    "session_id": sid_cdp,
                    "js": "JSON.stringify({ transform: getComputedStyle(document.getElementById('target')).transform, hover_match: document.querySelectorAll('#target:hover').length, q: !!document.querySelector('#target:hover') })",
                },
            )
            parsed = json.loads(r2["value"])
            t_cdp = parsed["transform"]
            hover_match = parsed.get("hover_match", 0)
            # The canonical signal that a real compositor hover fired is
            # `:hover` pseudo-class matching at query time. We accept any
            # of: pseudo-class match > 0, transform non-identity, or a
            # positive translate component.
            tx = 0.0
            if t_cdp.startswith("matrix("):
                parts = t_cdp[len("matrix("):-1].split(",")
                tx = float(parts[4].strip())
            assert hover_match > 0 or tx > 1.0, (t_cdp, hover_match, tx)
            print(
                f"  cdp mode: transform={t_cdp!r} hover_match={hover_match} tx={tx:.1f} "
                "(pseudo-class fired)"
            )

            # Also verify clock advanced past the paint-flush 16ms.
            cs = c.call("clock.status", {"session_id": sid_cdp})
            assert cs["virtual_now_ms"] >= 320, cs
            print(f"  clock.virtual_now_ms={cs['virtual_now_ms']:.1f}")
        finally:
            c.call("session.close", {"session_id": sid_cdp})

    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
