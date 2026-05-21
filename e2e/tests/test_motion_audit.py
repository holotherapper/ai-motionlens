#!/usr/bin/env python3
"""motion.audit_required: detect whether a page carries time-dependent UI."""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def audit(c: McpClient, fixture: str) -> dict:
    sid = c.call(
        "session.launch",
        {"url": fixture_url(fixture), "headless": True},
    )["session_id"]
    try:
        return c.call("motion.audit_required", {"session_id": sid})
    finally:
        c.call("session.close", {"session_id": sid})


def main() -> int:
    with McpClient() as c:
        css = audit(c, "css-animation.html")
        assert css["motion_detected"] is True, css
        assert css["required_verification"] is True, css
        assert len(css["recommended_target_times_ms"]) > 0, css
        # `sources` is the full TimelineSources struct
        assert (
            len(css["sources"]["css_animations"]["items"])
            + len(css["sources"]["css_transitions"]["items"])
            + len(css["sources"]["waapi"]["items"])
            >= 1
        ), css
        print(
            f"  audit(css-animation.html): motion_detected=True "
            f"recommended_target_times_ms={css['recommended_target_times_ms']}"
        )

        # broken-layout.html is a static page with no animations / transitions.
        none = audit(c, "broken-layout.html")
        assert none["motion_detected"] is False, none
        assert none["required_verification"] is False, none
        assert none["recommended_target_times_ms"] == [], none
        print(f"  audit(broken-layout.html): motion_detected=False")
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
