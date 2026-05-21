#!/usr/bin/env python3
"""motion.verify: the Animation Evidence Gate.

Cases:
- modal.html with a click trigger and an intent → passes:true (real fade)
- broken-layout.html with no triggers and `require_non_static: true`
  → passes:false (sequence_is_static evidence_missing)
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


def main() -> int:
    with McpClient() as c:
        # === case 1: modal fade should pass ===
        contract_pass = {
            "url": fixture_url("modal.html"),
            "viewport": {
                "width": 1280,
                "height": 800,
                "device_scale_factor": 1.0,
                "headless": True,
            },
            "episode_intent": {
                "description": "Modal fades in and slides up over ~300ms when #open-modal is clicked.",
                "expected_duration_ms": 300,
                "expected_kinds": ["fade", "translate"],
                "expected_targets": ["#modal"],
                "forbidden_kinds": ["disappearance"],
            },
            "triggers": [
                {
                    "at_t_ms": 0,
                    "kind": {
                        "kind": "click",
                        "target": {
                            "selector": "#open-modal",
                            "frame_path": [],
                            "resolved_coordinates": None,
                        },
                    },
                    "wait_policy": "none",
                }
            ],
            "sample_plan": {
                "target_times_ms": [0, 60, 150, 240, 320],
                "include_layout": True,
            },
            "thresholds": {
                "min_smoothness": 0.6,
                "max_jank_events": 0,
                "require_intent_match": True,
                "min_coverage_score": 0.0,
                "require_non_static": True,
            },
            "include_contact_sheet": False,
        }
        r = c.call("motion.verify", contract_pass)
        # Surface the report so a failure is debuggable from the log.
        if r["passes"] is not True:
            print("FAIL pass-case report dump:")
            print(f"  verdict={r['verdict']} verdict_human={r['verdict_human']}")
            print(f"  smoothness={r['assessment']['smoothness']:.3f} ({r['assessment']['smoothness_verdict']}) is_static={r['assessment']['is_static']}")
            print(f"  jank_events={r['assessment']['jank_events']}")
            print(f"  intent_match={r['assessment']['intent_match']}")
            print(f"  coverage_score={r['coverage_score']:.3f}  evidence_missing={r['evidence_missing']}")
            print(f"  motion_sources_without_samples={r['motion_sources_without_samples']}")
        assert r["passes"] is True, f"expected passes=True got {r['passes']}; verdict_human={r['verdict_human']}"
        assert r["assessment"]["is_static"] is False, r["assessment"]
        assert r["verdict"] in {"pass", "needs-attention"}, r["verdict"]
        assert isinstance(r["verdict_human"], str) and len(r["verdict_human"]) > 0, r["verdict_human"]
        im = r["assessment"]["intent_match"]
        assert im is not None and im["passes"] is True, im
        assert "#modal" in im["expected_targets_seen"], im
        assert "motionlens-report.json" in r["artifact_local_path"], r["artifact_local_path"]
        # artifact_local_path must always point at a real file — that's the
        # canonical Read-anywhere path the agent relies on.
        assert pathlib.Path(r["artifact_local_path"]).exists(), r["artifact_local_path"]
        # cwd_report_path is advisory: present only when the best-effort cwd
        # copy was written.  When set, it must also exist on disk.
        cwd_p = r.get("cwd_report_path")
        if cwd_p is not None:
            assert pathlib.Path(cwd_p).exists(), cwd_p
        assert r["contract_url"].endswith("modal.html"), r["contract_url"]
        print(
            f"  pass case: verdict={r['verdict']} smoothness={r['assessment']['smoothness']:.2f} "
            f"intent.passes={im['passes']} confidence={r['confidence']:.2f}"
        )
        print(f"  verdict_human: {r['verdict_human']}")

        # === case 2: static page must fail when require_non_static=true ===
        contract_fail = {
            "url": fixture_url("broken-layout.html"),
            "viewport": {
                "width": 1280,
                "height": 800,
                "device_scale_factor": 1.0,
                "headless": True,
            },
            "sample_plan": {
                "target_times_ms": [0, 100, 200],
                "include_layout": True,
            },
            "thresholds": {
                "min_smoothness": 0.6,
                "max_jank_events": 0,
                "require_intent_match": False,
                "min_coverage_score": 0.0,
                "require_non_static": True,
            },
            "include_contact_sheet": False,
        }
        r2 = c.call("motion.verify", contract_fail)
        assert r2["passes"] is False, r2
        assert r2["verdict"] == "fail", r2["verdict"]
        assert "static" in r2["verdict_human"].lower(), r2["verdict_human"]
        assert r2["assessment"]["is_static"] is True, r2["assessment"]
        assert r2["assessment"]["smoothness_verdict"] == "no-motion", r2["assessment"]
        assert any(
            "sequence_is_static" in s for s in r2["evidence_missing"]
        ), r2["evidence_missing"]
        print(f"  fail case: verdict={r2['verdict']} is_static=True")
        print(f"  verdict_human: {r2['verdict_human']}")
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
