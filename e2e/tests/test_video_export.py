#!/usr/bin/env python3
"""motion.video_export: GIF and APNG containers must round-trip correctly.

Regression guard: GIF / APNG bytes must go through the video artifact path,
not the PNG-only `ArtifactStore::save` path that would produce artifacts
decoding as malformed PNGs. This test verifies both containers are saved
with the right extension and that the bytes carry the expected magic header.
"""
from __future__ import annotations

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


GIF_MAGIC = b"GIF8"
PNG_MAGIC = b"\x89PNG\r\n\x1a\n"
APNG_ACTL_CHUNK = b"acTL"


def assert_video(c: McpClient, sid: str, fids: list[str], fmt: str) -> None:
    out = c.call(
        "motion.video_export",
        {"session_id": sid, "frame_ids": fids, "format": fmt, "fps": 10},
    )
    assert out["format"] == fmt, out
    assert out["frame_count"] == len(fids)
    assert out["width"] > 0 and out["height"] > 0
    path = pathlib.Path(out["artifact_local_path"])
    assert path.exists(), f"video file missing: {path}"
    assert path.suffix == f".{fmt}", f"unexpected suffix: {path}"
    bytes_ = path.read_bytes()
    if fmt == "gif":
        assert bytes_.startswith(GIF_MAGIC), "not a valid GIF header"
    elif fmt == "apng":
        assert bytes_.startswith(PNG_MAGIC), "not a valid PNG header"
        # The acTL chunk must appear in a real APNG. Its absence would
        # indicate a fallback to a static first-frame PNG.
        assert APNG_ACTL_CHUNK in bytes_, "APNG missing acTL chunk (not animated)"
    else:
        raise AssertionError(f"unknown format {fmt}")


def main() -> int:
    with McpClient() as c:
        sid = c.call(
            "session.launch",
            {"url": fixture_url("css-animation.html"), "headless": True},
        )["session_id"]
        try:
            c.call("episode.start", {"session_id": sid})
            series = c.call(
                "frame.capture_series",
                {"session_id": sid, "target_times_ms": [0, 50, 100, 150]},
            )
            fids = [f["frame_id"] for f in series["frames"]]
            assert_video(c, sid, fids, "gif")
            assert_video(c, sid, fids, "apng")
            print("video_export: gif + apng OK")
        finally:
            c.call("session.close", {"session_id": sid})
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
