"""End-to-end test: spawn the worker as a real subprocess and round-trip a hello call.

This is what the Rust orchestrator does at runtime — proving it works from
Python rules out encoding issues introduced by shell pipes during manual testing.
"""

from __future__ import annotations

import json
import subprocess
import sys


def test_subprocess_hello_round_trip() -> None:
    request = {"jsonrpc": "2.0", "id": 1, "method": "hello", "params": {"name": "subproc"}}
    proc = subprocess.run(
        [sys.executable, "-m", "narrowmind_workers"],
        input=json.dumps(request) + "\n",
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=15,
        check=True,
    )
    lines = [line for line in proc.stdout.splitlines() if line]
    assert len(lines) == 1, f"expected 1 response line, got: {proc.stdout!r}"
    response = json.loads(lines[0])
    assert response["jsonrpc"] == "2.0"
    assert response["id"] == 1
    assert response["result"]["message"] == "hello, subproc"
    assert response["result"]["worker_pid"] > 0
