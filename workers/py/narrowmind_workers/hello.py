"""Round-trip diagnostic handler used by Phase 0 to prove orchestrator ↔ worker IPC works."""

from __future__ import annotations

import os
import platform
import sys
from typing import Any

from narrowmind_workers import __version__


def hello(params: dict[str, Any]) -> dict[str, Any]:
    """Return a structured greeting plus identifying info about this worker process.

    The orchestrator surfaces this through the Tauri debug button so the user
    can confirm the Rust → Python boundary is healthy on their machine.
    """

    name = params.get("name", "world")
    if not isinstance(name, str):
        name = str(name)
    return {
        "message": f"hello, {name}",
        "worker_version": __version__,
        "worker_pid": os.getpid(),
        "python_version": sys.version.split()[0],
        "platform": platform.platform(),
    }
