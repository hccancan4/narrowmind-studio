"""Minimal JSON-RPC 2.0 server speaking line-delimited JSON over a pair of streams.

The orchestrator spawns each worker as a long-lived subprocess and multiplexes
requests over stdio. Workers are stateless between requests: all project state
lives on disk. ML code is never invoked across the boundary directly; only
file paths and metadata cross it.
"""

from __future__ import annotations

import json
import logging
import sys
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any, TextIO

log = logging.getLogger(__name__)

Method = Callable[[dict[str, Any]], Any]
"""A handler takes a params dict and returns a JSON-serializable value."""


class error_codes:  # noqa: N801 — namespace, not a real class
    """Reserved JSON-RPC 2.0 error codes (https://www.jsonrpc.org/specification#error_object)."""

    PARSE_ERROR = -32700
    INVALID_REQUEST = -32600
    METHOD_NOT_FOUND = -32601
    INVALID_PARAMS = -32602
    INTERNAL_ERROR = -32603


@dataclass
class JsonRpcError(Exception):
    """Raised by handlers to send a structured error back to the caller."""

    code: int
    message: str
    data: Any = None

    def to_response(self) -> dict[str, Any]:
        body: dict[str, Any] = {"code": self.code, "message": self.message}
        if self.data is not None:
            body["data"] = self.data
        return body


class MethodRegistry:
    """Mutable map of method name → handler. Workers register methods at startup."""

    def __init__(self) -> None:
        self._methods: dict[str, Method] = {}

    def register(self, name: str, handler: Method) -> None:
        if name in self._methods:
            raise ValueError(f"method already registered: {name}")
        self._methods[name] = handler

    def get(self, name: str) -> Method | None:
        return self._methods.get(name)

    def names(self) -> list[str]:
        return sorted(self._methods)


class JsonRpcServer:
    """Single-threaded line-framed JSON-RPC server.

    Read one JSON object per line from ``stdin``, dispatch it, and write the
    response (also one line) to ``stdout``. Notifications (requests without an
    ``id``) get no response. All logging goes to ``stderr`` so it never
    interleaves with the RPC stream.
    """

    def __init__(
        self,
        registry: MethodRegistry,
        stdin: TextIO | None = None,
        stdout: TextIO | None = None,
    ) -> None:
        self._registry = registry
        self._stdin = stdin if stdin is not None else sys.stdin
        self._stdout = stdout if stdout is not None else sys.stdout

    def serve_forever(self) -> None:
        for raw in self._stdin:
            line = raw.rstrip("\r\n")
            if not line:
                continue
            response = self._handle_line(line)
            if response is not None:
                self._stdout.write(json.dumps(response, ensure_ascii=False) + "\n")
                self._stdout.flush()

    def _handle_line(self, line: str) -> dict[str, Any] | None:
        try:
            payload = json.loads(line)
        except json.JSONDecodeError as exc:
            log.warning("parse error: %s", exc)
            return _error_response(None, error_codes.PARSE_ERROR, f"parse error: {exc}")

        if not isinstance(payload, dict) or payload.get("jsonrpc") != "2.0":
            return _error_response(
                payload.get("id") if isinstance(payload, dict) else None,
                error_codes.INVALID_REQUEST,
                "invalid request: missing or wrong jsonrpc version",
            )

        request_id = payload.get("id")
        method_name = payload.get("method")
        params = payload.get("params") or {}

        if not isinstance(method_name, str):
            return _error_response(
                request_id, error_codes.INVALID_REQUEST, "invalid request: method must be a string"
            )
        if not isinstance(params, dict):
            return _error_response(
                request_id, error_codes.INVALID_PARAMS, "invalid params: must be an object"
            )

        handler = self._registry.get(method_name)
        if handler is None:
            if request_id is None:
                return None
            return _error_response(
                request_id, error_codes.METHOD_NOT_FOUND, f"method not found: {method_name}"
            )

        try:
            result = handler(params)
        except JsonRpcError as exc:
            if request_id is None:
                return None
            return {"jsonrpc": "2.0", "id": request_id, "error": exc.to_response()}
        except Exception as exc:  # noqa: BLE001 — convert any handler crash to a typed error
            log.exception("handler %s raised", method_name)
            if request_id is None:
                return None
            return _error_response(
                request_id,
                error_codes.INTERNAL_ERROR,
                f"handler raised {type(exc).__name__}: {exc}",
            )

        if request_id is None:
            return None
        return {"jsonrpc": "2.0", "id": request_id, "result": result}


def _error_response(request_id: Any, code: int, message: str) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}}


def serve_stdio(registry: MethodRegistry) -> None:
    """Convenience entry point: configure stderr logging and serve.

    Hijack ``sys.stdout`` and redirect it to ``stderr`` so any third-party noise
    (``tqdm`` progress bars from sentence-transformers / huggingface_hub, stray
    ``print()`` calls in workers, library banners) cannot poison the JSON-RPC
    frame stream. The captured real stdout is handed to ``JsonRpcServer`` as the
    private protocol channel.

    Discovered the hard way in Phase 3: sentence-transformers' "Loading weights"
    bar printed control chars (carriage returns + binary) to fd 1 the first time
    a worker called ``embed_one``, which made the Rust side see ``stream did not
    contain valid UTF-8`` and tear the worker down. Belt-and-suspenders fix:
    disable progress bars + redirect stdout, so future deps that don't honor the
    env var can't reintroduce the bug.
    """
    import os

    os.environ.setdefault("HF_HUB_DISABLE_PROGRESS_BARS", "1")
    os.environ.setdefault("TRANSFORMERS_NO_ADVISORY_WARNINGS", "1")

    # CRITICAL on Windows: default sys.stdout/sys.stderr encoding follows the system
    # locale (cp1252 on a Turkish/Western Windows install), not UTF-8. When the rag
    # worker writes JSON with `ensure_ascii=False`, Unicode chunk text containing
    # em dashes, smart quotes, accented characters, etc. gets encoded as cp1252 —
    # which produces single-byte values like 0x97 that the Rust BufReader (strict
    # UTF-8) chokes on with "stream did not contain valid UTF-8". This wasted ~an
    # hour in Phase 3 acceptance because the broken byte was past the first 400
    # bytes that early hex dumps showed. Always reconfigure to UTF-8 before serving.
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="strict")
    if hasattr(sys.stderr, "reconfigure"):
        sys.stderr.reconfigure(encoding="utf-8", errors="replace")

    proto_stdout = sys.stdout
    sys.stdout = sys.stderr  # any leak now lands in our log file, not the RPC stream

    logging.basicConfig(
        stream=sys.stderr,
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )
    JsonRpcServer(registry, stdout=proto_stdout).serve_forever()
