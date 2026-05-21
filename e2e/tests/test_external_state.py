#!/usr/bin/env python3
"""external_state pinning: random / crypto / timezone / locale / user_agent.

Two launches with the same `external_state_seed` must produce the same
`Math.random` / `crypto.getRandomValues` sequence. Pinning timezone,
locale, and user_agent must override the platform defaults so replays
across machines see the same values.
"""
from __future__ import annotations

import json
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


PINNED_POLICY = {
    "network": "live",
    "service_worker": "live",
    "web_socket": "live",
    "storage": "pinned",
    "random": "pinned",
    "crypto": "pinned",
    "timezone": "pinned",
    "locale": "pinned",
    "user_agent": "pinned",
    "third_party_iframes": "live",
    "media_devices": "disabled",
}


def sample_once(c: McpClient, seed: int) -> dict:
    sid = c.call(
        "session.launch",
        {
            "url": fixture_url("random-determinism.html"),
            "viewport_width": 1280,
            "viewport_height": 800,
            "headless": True,
            "external_state": PINNED_POLICY,
            "external_state_seed": seed,
        },
    )["session_id"]
    try:
        c.call("episode.start", {"session_id": sid})
        r = c.call(
            "frame.dom_query",
            {"session_id": sid, "js": "window.__sample"},
        )
        return json.loads(r["value"])
    finally:
        c.call("session.close", {"session_id": sid})


def main() -> int:
    with McpClient() as c:
        a = sample_once(c, seed=42)
        b = sample_once(c, seed=42)
        # Same seed → identical random sequences across launches.
        assert a["math_random"] == b["math_random"], (
            a["math_random"],
            b["math_random"],
        )
        assert a["crypto"] == b["crypto"], (a["crypto"], b["crypto"])

        # Different seed → different sequence (with overwhelming probability).
        d = sample_once(c, seed=12345)
        assert d["math_random"] != a["math_random"], (
            d["math_random"],
            a["math_random"],
        )

        # Pinned overrides must take effect.
        assert a["timezone"] == "UTC", a["timezone"]
        assert a["locale"] == "en-US", a["locale"]
        assert "ai-motionlens" in a["user_agent"], a["user_agent"]
        print(
            f"  seed=42 deterministic: math.random[0]={a['math_random'][0]:.6f} "
            f"crypto[0]={a['crypto'][0]} tz={a['timezone']} locale={a['locale']}"
        )
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
