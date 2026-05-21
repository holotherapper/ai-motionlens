#!/usr/bin/env python3
"""Regression guard: `session.launch` and `motion.suggest_intent` both
return `observed_title` / `observed_url` so a port collision / login
wall / redirect surfaces *before* the agent constructs a contract from
the wrong page.

`motion.verify` alone carrying those fields is not enough: a dev port
silently serving another project's bundle would only be caught at the
verify report stage — after the agent had already spent a turn calling
`motion.suggest_intent` against the wrong page.
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
            # session.launch must surface the actually-served page identity
            # so the agent can spot a port collision immediately.
            assert launch.get("session_id"), f"session_id missing: {launch}"
            title = launch.get("observed_title")
            observed_url = launch.get("observed_url")
            assert title is not None, (
                f"session.launch must return observed_title; got {launch}"
            )
            assert observed_url is not None and url in observed_url, (
                f"session.launch must return observed_url matching the "
                f"requested URL; got observed_url={observed_url!r}, "
                f"requested={url!r}"
            )
        finally:
            c.call("session.close", {"session_id": sid})

        suggest = c.call(
            "motion.suggest_intent",
            {
                "url": url,
                "viewport": {
                    "width": 1280,
                    "height": 800,
                    "device_scale_factor": 1.0,
                    "headless": True,
                },
                "probe_window_ms": 600,
                "probe_steps": 6,
            },
        )

    # suggest_intent must mirror the same early-warning fields.
    s_title = suggest.get("observed_title")
    s_url = suggest.get("observed_url")
    assert s_title is not None, (
        f"motion.suggest_intent must return observed_title; got "
        f"{list(suggest.keys())}"
    )
    assert s_url is not None and url in s_url, (
        f"motion.suggest_intent observed_url must match the requested "
        f"URL; got observed_url={s_url!r}, requested={url!r}"
    )

    print(
        f"OK  session.launch + motion.suggest_intent both surface "
        f"observed_title={title!r} / observed_url confirms requested URL"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
