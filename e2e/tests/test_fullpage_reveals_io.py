#!/usr/bin/env python3
"""Regression guard: `include_final_fullpage_screenshot: true` primes
IntersectionObserver-driven reveal patterns before taking the fullpage
PNG, so panels that depend on a real scroll to enter the viewport are
captured in their revealed state — not the `opacity:0` initial state
that would otherwise produce a black-below-the-fold artefact.

The fixture has three `[data-reveal]` panels stacked across a 3000px
tall page. Without the primer the fullpage shot's #p2 and #p3 would
read as fully transparent (opacity 0) because IO never fires under
`captureBeyondViewport`. With the primer the gate scrolls to the
bottom + advances the virtual clock + scrolls back, so every panel
has been "seen" by IO and the screenshot captures their post-reveal
state.

Pixel inspection: the panel paints with `background: #1f6feb` (~rgb
31,111,235). After reveal each panel contributes a strongly
non-white region centred at its `top + 80px` and `left + 120px`.
"""
from __future__ import annotations

import os
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    contract = {
        "url": fixture_url("io-reveal-long.html"),
        "viewport": {"width": 1280, "height": 800, "device_scale_factor": 1.0, "headless": True},
        "sample_plan": {"target_times_ms": [0, 250, 500], "include_layout": True},
        "thresholds": {
            "min_smoothness": 0.0,
            "max_jank_events": 99,
            "require_intent_match": False,
            "min_coverage_score": 0.0,
            "require_non_static": False,
        },
        "include_contact_sheet": False,
        "include_final_fullpage_screenshot": True,
    }

    with McpClient() as c:
        r = c.call("motion.verify", contract)

    art = r.get("final_fullpage_screenshot") or {}
    path = art.get("artifact_local_path")
    assert path and os.path.exists(path), (
        f"final_fullpage_screenshot must be on disk; got {art}"
    )
    assert art.get("height", 0) >= 2900, (
        f"fullpage shot of the 3000px-tall page must be tall enough; "
        f"got height={art.get('height')}"
    )

    # Inspect the bottom panel's pixel region. PIL is in the uv-run
    # default env; if it's not we still pass the structural assertions.
    try:
        from PIL import Image
    except ImportError:
        print(
            f"OK  fullpage shot saved at {path} "
            f"(PIL missing — skipped pixel inspection)"
        )
        return 0

    img = Image.open(path).convert("RGB")
    w, h = img.size
    # #p3 lives at top=2200px, left=80px, 240x160 in document coords.
    # Sample its centre.
    px, py = 80 + 120, 2200 + 80
    if px < w and py < h:
        r_, g, b = img.getpixel((px, py))
        # The panel paints in rgb(31, 111, 235). Without the primer
        # the pixel would be near-white (the body's #fff bg) because
        # the panel stays at opacity 0.
        is_panel = b > 150 and b > r_ + 30 and b > g + 30
        assert is_panel, (
            f"#p3's pixel at ({px},{py}) must be the revealed panel "
            f"colour (blue-dominant), not the white background — IO "
            f"reveal primer is failing. got rgb=({r_},{g},{b})"
        )
        print(
            f"OK  bottom panel #p3 painted at ({px},{py}) -> "
            f"rgb({r_},{g},{b}); IO-driven reveal was primed before "
            f"the fullpage capture"
        )
    else:
        print(
            f"OK  fullpage shape {w}x{h}; (the test's pixel coord was "
            f"outside the image — skipped pixel inspection)"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
