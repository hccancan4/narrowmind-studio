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


def test_subprocess_non_ascii_param_round_trip() -> None:
    """Regression: the worker must decode stdin as UTF-8, not the Windows locale
    (cp1252). With raw non-ASCII bytes on the wire (ensure_ascii=False), a name
    containing 'ü' round-trips intact ONLY if serve_stdio reconfigured sys.stdin.

    Before the fix, on a Turkish/Western Windows install the worker decoded the
    UTF-8 bytes of 'ü' (0xC3 0xBC) as cp1252 -> 'Ã¼', so a project_root under
    `…\\Masaüstü\\…` arrived as mojibake and rag.query failed with
    'project_root is not a directory'. On Linux/macOS stdin is already UTF-8, so
    this is a no-op guard there and a real regression test on the reference box.
    """
    name = "Masaüstü-proje–ünïcode"  # 'ü', en-dash, diaeresis: must survive verbatim
    request = {"jsonrpc": "2.0", "id": 7, "method": "hello", "params": {"name": name}}
    proc = subprocess.run(
        [sys.executable, "-m", "narrowmind_workers"],
        # ensure_ascii=False puts the actual UTF-8 bytes on stdin (not \u escapes),
        # which is what exercises the child's stdin decoder.
        input=json.dumps(request, ensure_ascii=False) + "\n",
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=15,
        check=True,
    )
    lines = [line for line in proc.stdout.splitlines() if line]
    assert len(lines) == 1, f"expected 1 response line, got: {proc.stdout!r}"
    response = json.loads(lines[0])
    assert response["result"]["message"] == f"hello, {name}", (
        "non-ASCII stdin was mangled — serve_stdio must reconfigure sys.stdin to UTF-8"
    )
