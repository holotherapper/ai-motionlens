#!/usr/bin/env python3
"""Regression guard: a `performance.now()`-anchored rAF loop that stalls
under the virtual clock surfaces as a `raf-source-stalled` diagnosis
hint, naming the stuck target and telling the agent to switch the time
origin to the rAF `t` argument.

`performance.now()` does NOT advance in lock-step with the virtual rAF
tick, so a standard pattern (`const start = performance.now()` +
`performance.now() - start` inside the callback) keeps reading near-zero
and the loop never reaches its end condition. Any chained reveal that
waits on it stays dead. Without this hint the agent has to drag in a
real-time browser to figure out "is this my bug or the tool?" — the
hint moves that diagnosis into the report.

raf-stalled.html is a rAF page with a deliberately-static `#pending`
target alongside the moving loader / box / hero. The contract expects
`#pending` to fade in (it never does), so the hint condition
(rAF detected + intent target stuck + page non-static) is forced
deterministically — guarding the **mechanism and message**, not a
specific Chromium-environment `performance.now()` stall flavour. The
hint's message still teaches the rAF `t`-origin fix verbatim.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    contract = {
        "url": fixture_url("raf-stalled.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            # The page IS animating (loader / box / hero) AND a rAF
            # source is registered, but `#pending` deliberately never
            # moves. That triggers the raf-source-stalled hint condition
            # without depending on whether a particular `performance.now()`
            # rAF loop happens to stall in headless Chrome (it doesn't,
            # always — the page-level stall is environment-sensitive).
            # The hint's MESSAGE still teaches the fix; we are guarding
            # the mechanism, not a specific stall flavour.
            "description": "rAF page where #pending is expected to fade in but never does.",
            "expected_duration_ms": 1300,
            "expected_kinds": ["fade"],
            "expected_targets": ["#pending"],
        },
        "sample_plan": {"target_times_ms": [0, 250, 500, 750, 1200], "include_layout": True},
        "thresholds": {
            "min_smoothness": 0.0,
            "max_jank_events": 99,
            "require_intent_match": True,
            "min_coverage_score": 0.0,
            "require_non_static": True,
        },
        "include_contact_sheet": False,
    }
    with McpClient() as c:
        r = c.call("motion.verify", contract)

    hints = r.get("diagnosis_hints") or []
    rss = [h for h in hints if h.get("code") == "raf-source-stalled"]
    assert rss, (
        f"expected raf-source-stalled hint; got codes={[h.get('code') for h in hints]}"
    )
    h = rss[0]

    # It must name the stuck hero (or carry it in observed) so the agent
    # knows exactly which expected_target the rAF loop is gating.
    obs = h.get("observed") or {}
    stuck = obs.get("intent_targets_stuck") or []
    target = h.get("target_selector") or ""
    assert any("pending" in s for s in stuck) or "pending" in target, (
        f"hint must point at the stuck #pending; target={target!r} stuck={stuck}"
    )
    assert obs.get("raf_detected") is True, f"observed.raf_detected must be true: {obs}"
    assert obs.get("is_static") is False, f"is_static guard must be false: {obs}"

    # The message must teach the fix (switch the time origin to the rAF
    # callback `t` argument) without the agent having to read the skill.
    msg = h.get("message") or ""
    for needle in ("performance.now()", "rAF", "time origin"):
        assert needle in msg, f"message must mention {needle!r}: {msg!r}"

    assert h.get("suggested_probe"), "hint needs a suggested_probe"

    print(
        f"OK  target={target!r}  stuck={stuck}  "
        f"raf_active={obs.get('raf_active_count')}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
