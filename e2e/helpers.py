"""Shared helpers for ai-motionlens E2E tests.

Spawns the release MCP server binary, drives it via JSON-RPC over stdio.
"""
from __future__ import annotations

import json
import os
import pathlib
import subprocess
from typing import Any, Optional

REPO_ROOT = pathlib.Path(__file__).resolve().parents[1]
MCP_BIN = REPO_ROOT / "target" / "release" / "ai-motionlens-mcp"
FIXTURE_DIR = REPO_ROOT / "e2e" / "fixtures"


def fixture_url(name: str) -> str:
    """Resolve a fixture filename to a file:// URL."""
    p = FIXTURE_DIR / name
    if not p.exists():
        raise FileNotFoundError(p)
    return p.as_uri()


class McpClient:
    """Minimal MCP stdio client for tests."""

    def __init__(self, binary: Optional[pathlib.Path] = None):
        bin_path = binary or MCP_BIN
        if not bin_path.exists():
            raise FileNotFoundError(
                f"{bin_path} not found - run `cargo build --release` first"
            )
        self.proc = subprocess.Popen(
            [str(bin_path)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            bufsize=0,
        )
        self._id = 0
        self._initialize()

    def _next_id(self) -> int:
        self._id += 1
        return self._id

    def _send(self, method: str, params: Any = None, id_: Optional[int] = None) -> None:
        msg = {"jsonrpc": "2.0", "method": method}
        if id_ is not None:
            msg["id"] = id_
        if params is not None:
            msg["params"] = params
        self.proc.stdin.write((json.dumps(msg) + "\n").encode())
        self.proc.stdin.flush()

    def _recv(self) -> dict:
        line = self.proc.stdout.readline()
        if not line:
            raise RuntimeError("MCP server closed stdout unexpectedly")
        return json.loads(line.decode())

    def _initialize(self) -> None:
        rid = self._next_id()
        self._send(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "ai-motionlens-e2e", "version": "0"},
            },
            id_=rid,
        )
        while True:
            m = self._recv()
            if "id" in m and m["id"] == rid:
                if "error" in m:
                    raise RuntimeError(f"initialize: {m['error']}")
                break
        self._send("notifications/initialized")

    def call(self, name: str, args: dict) -> Any:
        rid = self._next_id()
        self._send("tools/call", {"name": name, "arguments": args}, id_=rid)
        while True:
            m = self._recv()
            if "id" in m and m["id"] == rid:
                if "error" in m:
                    raise RuntimeError(f"{name}: {m['error']}")
                text = m["result"]["content"][0]["text"]
                return json.loads(text)

    def tools_list(self) -> list[str]:
        rid = self._next_id()
        self._send("tools/list", id_=rid)
        while True:
            m = self._recv()
            if "id" in m and m["id"] == rid:
                return [t["name"] for t in m["result"]["tools"]]

    def read_resource(self, uri: str) -> dict:
        """Call MCP `resources/read` and return the first ResourceContents."""
        rid = self._next_id()
        self._send("resources/read", {"uri": uri}, id_=rid)
        while True:
            m = self._recv()
            if "id" in m and m["id"] == rid:
                if "error" in m:
                    raise RuntimeError(f"resources/read: {m['error']}")
                return m["result"]["contents"][0]

    def close(self) -> None:
        self.proc.stdin.close()
        self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()

    def __enter__(self) -> "McpClient":
        return self

    def __exit__(self, *_exc) -> None:
        self.close()
