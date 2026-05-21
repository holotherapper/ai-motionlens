#!/usr/bin/env python3
"""library.gsap_state / lenis_state / lottie_state must surface structured
state for pages that expose those globals.
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
            {"url": fixture_url("libraries.html"), "headless": True},
        )["session_id"]
        try:
            c.call("episode.start", {"session_id": sid})

            gsap = c.call("library.gsap_state", {"session_id": sid})["value"]
            assert gsap["available"] is True, gsap
            assert gsap["master"]["progress"] == 0.5
            assert gsap["scroll_triggers"], "expected one ScrollTrigger"
            assert gsap["scroll_triggers"][0]["scrub"] is True

            lenis = c.call("library.lenis_state", {"session_id": sid})["value"]
            assert lenis["available"] is True, lenis
            assert lenis["scroll"] == 120
            assert lenis["velocity"] == 8

            lottie = c.call("library.lottie_state", {"session_id": sid})["value"]
            assert lottie["available"] is True, lottie
            assert lottie["count"] == 1
            player = lottie["players"][0]
            assert player["currentFrame"] == 30
            assert player["totalFrames"] == 60
            print("library state: gsap / lenis / lottie all reported")
        finally:
            c.call("session.close", {"session_id": sid})
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
