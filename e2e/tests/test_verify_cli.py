#!/usr/bin/env python3
"""ai-motionlens verify CLI: contract in -> report + exit code out.

Also exercises `ai-motionlens gate-check`, including the regression case
where the report's viewport does not match the gate-check config's
viewport — that mismatch must be detected and surfaced as exit 1.
"""
from __future__ import annotations

import copy
import json
import pathlib
import subprocess
import sys
import tempfile

REPO = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "e2e"))
from helpers import fixture_url

CLI_BIN = REPO / "target" / "release" / "ai-motionlens"


def _resolve_bin() -> pathlib.Path:
    if CLI_BIN.exists():
        return CLI_BIN
    debug = REPO / "target" / "debug" / "ai-motionlens"
    if debug.exists():
        return debug
    raise FileNotFoundError(
        f"{CLI_BIN} not found - run `cargo build --release` first"
    )


def run_cli(config: dict) -> tuple[int, bytes]:
    bin_path = _resolve_bin()
    with tempfile.TemporaryDirectory() as tmp:
        cfg_path = pathlib.Path(tmp) / "motionlens.config.json"
        out_path = pathlib.Path(tmp) / "motionlens-report.json"
        cfg_path.write_text(json.dumps(config))
        proc = subprocess.run(
            [str(bin_path), "verify", "--config", str(cfg_path), "--out", str(out_path)],
            capture_output=True,
            timeout=120,
            cwd=tmp,
        )
        return proc.returncode, out_path.read_bytes() if out_path.exists() else b""


def run_verify_then_gate(
    verify_cfg: dict, gate_cfg: dict | None = None
) -> tuple[int, int, str]:
    """Run verify and then gate-check inside a fresh tempdir.

    Returns (verify_rc, gate_rc, gate_stderr). When gate_cfg is None the
    same config is used on both sides — gate-check must exit 0. When
    gate_cfg differs (e.g. viewport changed), gate-check must exit 1 and
    surface the mismatch on stderr.
    """
    bin_path = _resolve_bin()
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = pathlib.Path(tmp)
        cfg_path = tmp_path / "motionlens.config.json"
        report_path = tmp_path / "motionlens-report.json"
        gate_cfg_path = tmp_path / "gate-check.config.json"
        cfg_path.write_text(json.dumps(verify_cfg))
        gate_cfg_path.write_text(json.dumps(gate_cfg if gate_cfg is not None else verify_cfg))
        verify_proc = subprocess.run(
            [str(bin_path), "verify", "--config", str(cfg_path), "--out", str(report_path)],
            capture_output=True,
            timeout=120,
            cwd=tmp,
        )
        gate_proc = subprocess.run(
            [
                str(bin_path),
                "gate-check",
                "--report",
                str(report_path),
                "--config",
                str(gate_cfg_path),
                "--skip-freshness",
            ],
            capture_output=True,
            timeout=30,
            cwd=tmp,
        )
        return (
            verify_proc.returncode,
            gate_proc.returncode,
            gate_proc.stderr.decode("utf-8", errors="replace"),
        )


def main() -> int:
    # Pass case
    cfg_pass = {
        "url": fixture_url("modal.html"),
        "viewport": {
            "width": 1280,
            "height": 800,
            "device_scale_factor": 1.0,
            "headless": True,
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
            "min_smoothness": 0.2,
            "max_jank_events": 5,
            "require_intent_match": False,
            "min_coverage_score": 0.0,
            "require_non_static": True,
        },
        "include_contact_sheet": False,
    }
    rc, raw = run_cli(cfg_pass)
    assert rc == 0, f"expected exit 0, got {rc}"
    report = json.loads(raw)
    assert report["passes"] is True, report
    print(f"  pass: rc=0 passes=True smoothness={report['assessment']['smoothness']:.2f}")

    # Fail case (static page)
    cfg_fail = {
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
    rc2, raw2 = run_cli(cfg_fail)
    assert rc2 == 1, f"expected exit 1, got {rc2}"
    report2 = json.loads(raw2)
    assert report2["passes"] is False, report2
    print(f"  fail: rc=1 passes=False is_static={report2['assessment']['is_static']}")

    # gate-check: matching config must pass.
    v_rc, g_rc, g_err = run_verify_then_gate(cfg_pass)
    assert v_rc == 0, f"verify rc {v_rc}"
    assert g_rc == 0, f"gate-check (matching) expected 0, got {g_rc}; stderr={g_err}"
    print(f"  gate-check matching: rc=0")

    # gate-check: viewport mismatch must be detected.  This is the
    # regression test for the bug where gate-check inspected
    # `report["viewport"]` (which does not exist) instead of
    # `report["contract_viewport"]`, so the mismatch went undetected and
    # the gate silently approved any size.
    gate_cfg_bad_viewport = copy.deepcopy(cfg_pass)
    gate_cfg_bad_viewport["viewport"]["width"] = 375
    gate_cfg_bad_viewport["viewport"]["height"] = 667
    v_rc, g_rc, g_err = run_verify_then_gate(cfg_pass, gate_cfg_bad_viewport)
    assert v_rc == 0, f"verify rc {v_rc}"
    assert g_rc == 1, (
        f"gate-check (viewport mismatch) expected 1, got {g_rc}; stderr={g_err}"
    )
    assert "viewport" in g_err.lower(), f"stderr does not mention viewport: {g_err!r}"
    print(f"  gate-check viewport mismatch: rc=1 stderr OK")

    # gate-check: url mismatch must be detected too.
    gate_cfg_bad_url = copy.deepcopy(cfg_pass)
    gate_cfg_bad_url["url"] = fixture_url("broken-layout.html")
    v_rc, g_rc, g_err = run_verify_then_gate(cfg_pass, gate_cfg_bad_url)
    assert v_rc == 0, f"verify rc {v_rc}"
    assert g_rc == 1, f"gate-check (url mismatch) expected 1, got {g_rc}; stderr={g_err}"
    assert "url" in g_err.lower(), f"stderr does not mention url: {g_err!r}"
    print(f"  gate-check url mismatch: rc=1 stderr OK")

    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
