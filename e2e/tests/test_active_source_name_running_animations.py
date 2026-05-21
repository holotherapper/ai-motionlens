#!/usr/bin/env python3
"""Regression guard: `frame.active_sources[].name` is populated for CSS
animations, and the same animation name appears in
`layout_snapshot.elements[].running_animations` for the element it
actually runs on. Cross-referencing the two lets agents identify which
element a hashed `cssId` target_selector points to — `name` plus
`running_animations` close the loop without leaking backend node ids.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    url = fixture_url("entrance-from-zero.html")

    with McpClient() as c:
        launch = c.call(
            "session.launch",
            {"url": url, "viewport_width": 1280, "viewport_height": 800, "headless": True},
        )
        sid = launch["session_id"]
        try:
            c.call("episode.start", {"session_id": sid})
            # Advance halfway through the 600ms `fade-in` so the
            # animation is in `running` state.
            c.call("clock.advance", {"session_id": sid, "delta_ms": 200})
            frame = c.call(
                "frame.capture",
                {
                    "session_id": sid,
                    "format": "png",
                    "thumbnail": False,
                    "layout": {"match_selectors": ["#eyebrow"]},
                },
            )
        finally:
            c.call("session.close", {"session_id": sid})

    active = frame.get("active_sources") or []
    layout = frame.get("layout_snapshot") or {}
    elements = layout.get("elements") or []

    # The CDP Animation domain should report at least one source
    # named `fade-in` (the fixture's @keyframes).
    names = [s.get("name") for s in active if s.get("name")]
    assert "fade-in" in names, (
        f"active_sources must carry the CSS animation name; got "
        f"names={names}, full active_sources={active}"
    )

    # The element actually running `fade-in` must report it in
    # `running_animations`, so a hashed `target_selector` can be
    # cross-referenced back to the selector.
    eyebrow = next(
        (e for e in elements if "#eyebrow" in (e.get("matched_selectors") or []) or e.get("selector") == "#eyebrow"),
        None,
    )
    assert eyebrow is not None, (
        f"#eyebrow must appear in layout snapshot (bypassed via "
        f"match_selectors); got selectors={[e.get('selector') for e in elements]}"
    )
    running = eyebrow.get("running_animations") or []
    assert "fade-in" in running, (
        f"#eyebrow.running_animations must include `fade-in` from "
        f"`el.getAnimations()`; got {running}"
    )

    print(
        f"OK  active_sources[].name={names} matches "
        f"#eyebrow.running_animations={running} — agent can identify "
        f"the hashed target_selector via the animation name"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
