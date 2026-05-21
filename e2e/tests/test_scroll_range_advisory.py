#!/usr/bin/env python3
"""Regression guard for two scroll-range behaviours:

(1) `scroll_range_match.verdict != "match"` MUST NOT flip
    `intent_match.passes` to false. The mismatch is a contract-vs-
    observation alignment issue (often a `position: sticky` / pinned
    section whose ScrollTrigger active range differs from the bbox
    progress the recipe samples) — that is not a correctness defect.
    Agents read the `scroll_range_match.verdict` field directly to
    decide whether the range mismatch matters; flipping `passes` would
    make a contract-vs-observation alignment gap look like an
    implementation bug.

(2) `recipes.scroll_animation_check` MUST self-start an episode when
    the caller hasn't — the recipe is the high-level entry point and
    should not require the agent to know about the underlying ledger.
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
            {
                "url": fixture_url("scroll-section.html"),
                "viewport_width": 1280,
                "viewport_height": 720,
                "headless": True,
            },
        )["session_id"]
        try:
            c.call("episode.start", {"session_id": sid})
            c.call(
                "episode.set_intent",
                {
                    "session_id": sid,
                    "description": "Hero scales/rotates across part of the section.",
                    # Declare a wider expected range than what we'll
                    # actually sample — produces a
                    # `narrower-than-expected` verdict.
                    "expected_progress_range": [0.0, 1.0],
                    "expected_kinds": ["scale", "rotate", "color-change"],
                },
            )
            r = c.call(
                "recipes.scroll_animation_check",
                {
                    "session_id": sid,
                    "progresses_in_section": {
                        "selector": "#scrub-section",
                        # Sample only the middle of the section so the
                        # observed range is [0.25 .. 0.75], narrower
                        # than the declared [0.0 .. 1.0].
                        "values": [0.25, 0.5, 0.75],
                    },
                    "target_selectors": ["#hero"],
                },
            )
            assess = r["assessment"]
            im = assess["intent_match"]
            srm = im.get("scroll_range_match") or {}

            assert srm.get("verdict") in (
                "narrower-than-expected",
                "outside-expected",
            ), f"sample setup must produce a non-match verdict; got {srm}"

            # The headline correctness signal must stay green: the
            # animation is fine, only the contract's range is wider
            # than the sampling window.
            assert im.get("passes") is True, (
                f"intent_match.passes must remain true when only the "
                f"scroll range diverges; got im={im}, srm={srm}, "
                f"failed_gates={r.get('failed_gates')}"
            )

            # The recipe also surfaces the mismatch in its own
            # `failed_gates` list as an *advisory* entry, mirroring the
            # `motion.verify` API shape so agents can grep one place
            # for "things to triage".
            fg = r.get("failed_gates") or []
            srm_gates = [g for g in fg if g.get("gate") == "scroll-range-match"]
            assert len(srm_gates) == 1, (
                f"failed_gates must include exactly one "
                f"`scroll-range-match` entry; got {fg}"
            )
            assert srm_gates[0].get("reliability") == "advisory", (
                f"scroll-range-match must be advisory, not correctness; "
                f"got {srm_gates[0]}"
            )
            print(
                f"OK  scroll_range_match verdict={srm['verdict']!r} → "
                f"intent_match.passes={im['passes']} + "
                f"failed_gates[scroll-range-match].reliability=advisory"
            )
        finally:
            c.call("session.close", {"session_id": sid})

    # Part 2: recipes.scroll_animation_check must self-start an episode
    # when none is active.
    with McpClient() as c:
        sid = c.call(
            "session.launch",
            {
                "url": fixture_url("scroll-section.html"),
                "viewport_width": 1280,
                "viewport_height": 720,
                "headless": True,
            },
        )["session_id"]
        try:
            # No episode.start, no set_intent — just hand it the URL
            # and the progress sampling, and the recipe must figure it
            # out.
            r = c.call(
                "recipes.scroll_animation_check",
                {
                    "session_id": sid,
                    "progresses_in_section": {
                        "selector": "#scrub-section",
                        "values": [0.0, 0.5, 1.0],
                    },
                    "target_selectors": ["#hero"],
                },
            )
            assert r["series"] is not None, (
                f"auto-start: recipe must succeed and return series; got {r}"
            )
            print("OK  recipes.scroll_animation_check auto-started episode")
        finally:
            c.call("session.close", {"session_id": sid})

    return 0


if __name__ == "__main__":
    sys.exit(main())
