"""JSON-RPC surface of the training worker.

- ``training.run``: blocking QLoRA fine-tune; streams ``training.metric`` /
  ``training.stage`` notifications throughout (the Rust side consumes them via
  ``call_worker_streaming`` with an activity-based deadline).
- ``training.test_adapter``: load base+adapter, answer a few questions (M6).

Failure contract: any exception is caught, written to status.json as
``status="failed"`` with the message, and re-raised as a JSON-RPC error so the
Rust side gets both the durable record AND the error response.
"""

from __future__ import annotations

import logging
import traceback
from pathlib import Path
from typing import Any

from narrowmind_workers.rpc import JsonRpcError, MethodRegistry, error_codes
from narrowmind_workers.training.config import TrainingParams
from narrowmind_workers.training.runner import (
    append_log,
    execute_export_gguf,
    execute_test_adapter,
    execute_training,
    run_dir,
    write_status,
)

log = logging.getLogger(__name__)


def register_methods(registry: MethodRegistry) -> None:
    registry.register("training.run", _run)
    registry.register("training.test_adapter", _test_adapter)
    registry.register("training.export_gguf", _export_gguf)


def _run(params: dict[str, Any]) -> dict[str, Any]:
    project_root = _required_dir(params, "project_root")
    run_id = _required_str(params, "run_id")
    resume_from = params.get("resume_from")
    if resume_from is not None and not isinstance(resume_from, str):
        raise JsonRpcError(error_codes.INVALID_PARAMS, "`resume_from` must be a string")

    try:
        tp = TrainingParams.from_params(params.get("config") or {})
    except ValueError as e:
        raise JsonRpcError(error_codes.INVALID_PARAMS, f"config: {e}") from e

    try:
        return execute_training(project_root, run_id, tp, resume_from)
    except Exception as e:  # noqa: BLE001 — boundary: persist + surface everything
        rd = run_dir(project_root, run_id)
        try:
            rd.mkdir(parents=True, exist_ok=True)
            write_status(rd, run_id=run_id, status="failed", error=str(e))
            append_log(rd, f"FAILED: {e}")
        except OSError:
            pass
        log.error("training.run failed: %s\n%s", e, traceback.format_exc())
        raise JsonRpcError(error_codes.INTERNAL_ERROR, f"training failed: {e}") from e


def _test_adapter(params: dict[str, Any]) -> dict[str, Any]:
    project_root = _required_dir(params, "project_root")
    run_id = _required_str(params, "run_id")
    questions = params.get("questions")
    if not isinstance(questions, list) or not questions or not all(
        isinstance(q, str) and q for q in questions
    ):
        raise JsonRpcError(error_codes.INVALID_PARAMS, "`questions` must be a non-empty string list")
    max_new_tokens = params.get("max_new_tokens", 256)
    if not isinstance(max_new_tokens, int) or not (16 <= max_new_tokens <= 2048):
        raise JsonRpcError(error_codes.INVALID_PARAMS, "`max_new_tokens` must be in [16, 2048]")

    try:
        return execute_test_adapter(project_root, run_id, questions, max_new_tokens)
    except Exception as e:  # noqa: BLE001
        log.error("training.test_adapter failed: %s\n%s", e, traceback.format_exc())
        raise JsonRpcError(error_codes.INTERNAL_ERROR, f"test_adapter failed: {e}") from e


def _export_gguf(params: dict[str, Any]) -> dict[str, Any]:
    project_root = _required_dir(params, "project_root")
    run_id = _required_str(params, "run_id")
    quant = params.get("quant", "Q4_K_M")
    if not isinstance(quant, str) or not quant:
        raise JsonRpcError(error_codes.INVALID_PARAMS, "`quant` must be a non-empty string")
    domain = params.get("domain")
    if domain is not None and (not isinstance(domain, str) or not domain):
        raise JsonRpcError(error_codes.INVALID_PARAMS, "`domain` must be a non-empty string")
    imatrix = bool(params.get("imatrix", False))
    imatrix_chunks = params.get("imatrix_chunks", 80)
    if not isinstance(imatrix_chunks, int) or not (1 <= imatrix_chunks <= 1000):
        raise JsonRpcError(
            error_codes.INVALID_PARAMS, "`imatrix_chunks` must be an int in [1, 1000]"
        )

    try:
        return execute_export_gguf(
            project_root,
            run_id,
            quant=quant,
            domain=domain,
            imatrix=imatrix,
            imatrix_chunks=imatrix_chunks,
        )
    except Exception as e:  # noqa: BLE001
        log.error("training.export_gguf failed: %s\n%s", e, traceback.format_exc())
        raise JsonRpcError(error_codes.INTERNAL_ERROR, f"export_gguf failed: {e}") from e


def _required_str(params: dict[str, Any], key: str) -> str:
    v = params.get(key)
    if not isinstance(v, str) or not v:
        raise JsonRpcError(error_codes.INVALID_PARAMS, f"missing or empty `{key}`")
    return v


def _required_dir(params: dict[str, Any], key: str) -> Path:
    p = Path(_required_str(params, key))
    if not p.is_dir():
        raise JsonRpcError(error_codes.INVALID_PARAMS, f"{key} is not a directory: {p}")
    return p
