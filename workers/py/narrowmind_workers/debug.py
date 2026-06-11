"""Debug-only RPC methods for exercising the Rust WorkerPool's failure paths.

These are registered ONLY when the ``NM_DEBUG_RPC=1`` environment variable is
set (the pool's integration tests set it via ``PythonRunner::with_env``).
Production spawns never see them, so there is no way for the agent or a user
prompt to invoke ``debug.exit`` against a live worker.

Methods:
- ``debug.sleep {seconds}``  — block the (single-threaded) server loop; lets
  tests trigger in-flight timeouts deterministically.
- ``debug.exit {code?}``     — hard-exit the process mid-call via ``os._exit``
  so no response is ever written; simulates a crashed handler.
- ``debug.spam_stderr {bytes?}`` — write ~N bytes of noise to stderr before
  responding; proves the Rust-side drain task prevents pipe-buffer deadlock.
"""

from __future__ import annotations

import os
import sys
import time
from typing import Any

from narrowmind_workers.rpc import MethodRegistry


def register_debug_methods(registry: MethodRegistry) -> None:
    """Register the debug surface iff NM_DEBUG_RPC=1. Call unconditionally —
    gating lives here so every worker entry point stays a one-liner."""
    if os.environ.get("NM_DEBUG_RPC") != "1":
        return
    registry.register("debug.sleep", _sleep)
    registry.register("debug.exit", _exit)
    registry.register("debug.spam_stderr", _spam_stderr)
    registry.register("debug.notify_storm", _notify_storm)


def _sleep(params: dict[str, Any]) -> dict[str, Any]:
    seconds = float(params.get("seconds", 1.0))
    time.sleep(seconds)
    return {"slept": seconds}


def _exit(params: dict[str, Any]) -> dict[str, Any]:
    # os._exit skips atexit/flush — the Rust side sees EOF with no response,
    # exactly like a segfaulted handler.
    code = int(params.get("code", 1))
    os._exit(code)


def _spam_stderr(params: dict[str, Any]) -> dict[str, Any]:
    n_bytes = int(params.get("bytes", 1_000_000))
    line = "x" * 200
    written = 0
    while written < n_bytes:
        print(line, file=sys.stderr)
        written += len(line) + 1
    sys.stderr.flush()
    return {"wrote_stderr_bytes": written}


def _notify_storm(params: dict[str, Any]) -> dict[str, Any]:
    """Emit ``count`` notifications spaced ``interval_ms`` apart, then return.

    The streaming-call test fixture: exercises the Rust side's id-matching
    read loop (skip id-less frames, surface them via callback) and the
    activity-based deadline (each notification must refresh the idle timer —
    a storm whose TOTAL duration exceeds the idle timeout but whose
    inter-notification gap stays under it must complete successfully).
    """
    from narrowmind_workers.rpc import notify

    count = int(params.get("count", 5))
    interval_ms = int(params.get("interval_ms", 0))
    for i in range(count):
        if interval_ms > 0:
            time.sleep(interval_ms / 1000.0)
        notify("debug.tick", {"seq": i, "of": count})
    return {"notified": count}
