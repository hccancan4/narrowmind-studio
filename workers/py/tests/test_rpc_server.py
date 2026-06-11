"""Integration tests for the JSON-RPC server using in-memory streams."""

from __future__ import annotations

import io
import json
from typing import Any

from narrowmind_workers.hello import hello
from narrowmind_workers.rpc import JsonRpcError, JsonRpcServer, MethodRegistry, error_codes


def _run(stdin_lines: list[str], registry: MethodRegistry) -> list[dict[str, Any]]:
    stdin = io.StringIO("\n".join(stdin_lines) + "\n")
    stdout = io.StringIO()
    JsonRpcServer(registry, stdin=stdin, stdout=stdout).serve_forever()
    return [json.loads(line) for line in stdout.getvalue().splitlines() if line]


def _hello_registry() -> MethodRegistry:
    registry = MethodRegistry()
    registry.register("hello", hello)
    return registry


def test_round_trip_hello() -> None:
    request = {"jsonrpc": "2.0", "id": 1, "method": "hello", "params": {"name": "test"}}
    responses = _run([json.dumps(request)], _hello_registry())
    assert len(responses) == 1
    assert responses[0]["jsonrpc"] == "2.0"
    assert responses[0]["id"] == 1
    assert responses[0]["result"]["message"] == "hello, test"


def test_notification_yields_no_response() -> None:
    request = {"jsonrpc": "2.0", "method": "hello", "params": {}}
    responses = _run([json.dumps(request)], _hello_registry())
    assert responses == []


def test_unknown_method_returns_method_not_found() -> None:
    request = {"jsonrpc": "2.0", "id": 7, "method": "nope"}
    responses = _run([json.dumps(request)], _hello_registry())
    assert responses[0]["error"]["code"] == error_codes.METHOD_NOT_FOUND


def test_malformed_json_returns_parse_error() -> None:
    responses = _run(["{not json"], MethodRegistry())
    assert responses[0]["error"]["code"] == error_codes.PARSE_ERROR


def test_wrong_jsonrpc_version_returns_invalid_request() -> None:
    request = {"jsonrpc": "1.0", "id": 1, "method": "hello"}
    responses = _run([json.dumps(request)], _hello_registry())
    assert responses[0]["error"]["code"] == error_codes.INVALID_REQUEST


def test_handler_raising_json_rpc_error_returns_it() -> None:
    def boom(_params: dict[str, Any]) -> dict[str, Any]:
        raise JsonRpcError(code=-32001, message="custom failure")

    registry = MethodRegistry()
    registry.register("boom", boom)
    request = {"jsonrpc": "2.0", "id": 1, "method": "boom"}
    responses = _run([json.dumps(request)], registry)
    assert responses[0]["error"] == {"code": -32001, "message": "custom failure"}


def test_handler_raising_unexpected_returns_internal_error() -> None:
    def boom(_params: dict[str, Any]) -> dict[str, Any]:
        raise RuntimeError("oops")

    registry = MethodRegistry()
    registry.register("boom", boom)
    request = {"jsonrpc": "2.0", "id": 1, "method": "boom"}
    responses = _run([json.dumps(request)], registry)
    assert responses[0]["error"]["code"] == error_codes.INTERNAL_ERROR
    assert "oops" in responses[0]["error"]["message"]


# ---------------------------------------------------------------------------
# Long-lived serving contract (Rust WorkerPool reuses one child for many calls)
# ---------------------------------------------------------------------------


def test_multiple_requests_on_one_stream_respond_in_order() -> None:
    """The Rust WorkerPool keeps the child alive and pipes many requests down
    one stdin. The server must answer each in order with the matching id —
    a regression here silently cross-wires responses between callers."""
    requests = [
        {"jsonrpc": "2.0", "id": 10, "method": "hello", "params": {"name": "first"}},
        {"jsonrpc": "2.0", "id": 11, "method": "hello", "params": {"name": "second"}},
        {"jsonrpc": "2.0", "id": 12, "method": "hello", "params": {"name": "third"}},
    ]
    responses = _run([json.dumps(r) for r in requests], _hello_registry())
    assert [r["id"] for r in responses] == [10, 11, 12]
    assert [r["result"]["message"] for r in responses] == [
        "hello, first",
        "hello, second",
        "hello, third",
    ]


def test_second_request_with_multibyte_utf8_survives_reuse() -> None:
    """Phase 3's cp1252 incident was caught on a fresh process; under reuse the
    encoding state must hold for EVERY subsequent frame too (em dash, Turkish
    chars, CJK)."""
    requests = [
        {"jsonrpc": "2.0", "id": 1, "method": "hello", "params": {"name": "plain"}},
        {"jsonrpc": "2.0", "id": 2, "method": "hello", "params": {"name": "Übermensch — 形存則神存"}},
    ]
    responses = _run([json.dumps(r, ensure_ascii=False) for r in requests], _hello_registry())
    assert responses[1]["result"]["message"] == "hello, Übermensch — 形存則神存"
