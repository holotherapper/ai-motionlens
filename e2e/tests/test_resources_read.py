#!/usr/bin/env python3
"""MCP `resources/read` must return the full-resolution PNG blob for a frame
artifact URI emitted by capture tools.
"""
from __future__ import annotations

import base64
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from helpers import McpClient, fixture_url


PNG_MAGIC = b"\x89PNG\r\n\x1a\n"


def main() -> int:
    with McpClient() as c:
        sid = c.call(
            "session.launch",
            {"url": fixture_url("modal.html"), "headless": True},
        )["session_id"]
        try:
            c.call("episode.start", {"session_id": sid})
            f = c.call("frame.capture", {"session_id": sid})
            uri = f["artifact_uri"]
            assert uri.startswith("motionlens://artifacts/"), uri

            payload = c.read_resource(uri)
            assert payload["uri"] == uri
            assert payload["mimeType"] == "image/png", payload
            blob_b64 = payload["blob"]
            decoded = base64.b64decode(blob_b64)
            assert decoded.startswith(PNG_MAGIC), "resources/read did not return a PNG"
            local = pathlib.Path(f["artifact_local_path"]).read_bytes()
            assert decoded == local, "MCP blob differs from on-disk artifact"
            print(f"resources/read: {len(decoded)} bytes, matches local path")
        finally:
            c.call("session.close", {"session_id": sid})
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
