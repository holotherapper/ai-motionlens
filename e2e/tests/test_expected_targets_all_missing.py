#!/usr/bin/env python3
"""Regression guard: when every `expected_target` is absent from every
captured layout, a meta-cause hint surfaces it as the leading candidate.
The real cause is often a page-level failure — e.g. a port collision
serving a different site at the same URL — that N individual
`selector-not-found` hints would otherwise obscure.

`expected-targets-all-missing` lists wrong-URL / page-not-reached /
login-wall / contract-drift as the meta-causes to check first, so the
agent sees the page-level diagnosis before drilling per-selector.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    contract = {
        # css-animation.html DOES animate `.box`, but the contract
        # declares THREE targets that don't exist on it — every declared
        # expected_target is absent from every layout snapshot, exactly
        # the "wrong URL / contract drift" symptom shape.
        "url": fixture_url("css-animation.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "episode_intent": {
            "description": "All targets are deliberately bogus.",
            "expected_duration_ms": 1000,
            "expected_kinds": ["translate"],
            "expected_targets": ["#does-not-exist-a", "#does-not-exist-b", ".not-here"],
        },
        "sample_plan": {"target_times_ms": [0, 250, 500, 1000], "include_layout": True},
        "thresholds": {
            "min_smoothness": 0.0,
            "max_jank_events": 99,
            "require_intent_match": False,
            "min_coverage_score": 0.0,
            "require_non_static": True,
        },
        "include_contact_sheet": False,
    }
    with McpClient() as c:
        r = c.call("motion.verify", contract)

    hints = r.get("diagnosis_hints") or []
    codes = [h.get("code") for h in hints]
    meta = next((h for h in hints if h.get("code") == "expected-targets-all-missing"), None)
    assert meta, f"expected expected-targets-all-missing meta hint; got {codes}"

    # The meta-hint should mention the most useful meta-causes inline so
    # the agent can rule them out without reading the skill doc.
    msg = meta.get("message") or ""
    for needle in ("URL is serving unexpected", "auth", "contract drift"):
        assert needle in msg, f"meta message must enumerate meta-causes, missing {needle!r}: {msg!r}"

    obs = meta.get("observed") or {}
    assert obs.get("all_missing_from_layout") is True, f"observed.all_missing_from_layout: {obs}"
    assert len(obs.get("expected_targets") or []) == 3, f"observed must echo expected_targets: {obs}"
    assert meta.get("suggested_probe"), "meta hint needs a suggested_probe"

    # The meta hint must come BEFORE the individual selector-not-found
    # hints — the whole point of surfacing the meta-cause first.
    meta_idx = codes.index("expected-targets-all-missing")
    snf_idxs = [i for i, c in enumerate(codes) if c == "selector-not-found"]
    assert snf_idxs and meta_idx < min(snf_idxs), (
        f"meta hint must precede per-selector hints; codes={codes}"
    )

    print(f"OK  codes={codes}  needles ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
