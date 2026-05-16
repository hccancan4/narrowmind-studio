"""Unit tests for the hello handler."""

from __future__ import annotations

from narrowmind_workers import __version__
from narrowmind_workers.hello import hello


def test_hello_default_name() -> None:
    result = hello({})
    assert result["message"] == "hello, world"
    assert result["worker_version"] == __version__
    assert isinstance(result["worker_pid"], int)
    assert result["worker_pid"] > 0


def test_hello_named() -> None:
    result = hello({"name": "Hasancan"})
    assert result["message"] == "hello, Hasancan"


def test_hello_coerces_non_string_name() -> None:
    result = hello({"name": 42})
    assert result["message"] == "hello, 42"
