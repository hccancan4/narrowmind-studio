"""JSON-RPC entry points exposed by the ingestion worker.

Method names are the contract the Rust orchestrator calls. Keep the args + return shape
stable — bump method names rather than silently change semantics.
"""

from __future__ import annotations

import logging
from pathlib import Path
from typing import Any

from narrowmind_workers.ingestion.local import ingest_local_path
from narrowmind_workers.rpc import JsonRpcError, MethodRegistry, error_codes

log = logging.getLogger(__name__)


def register_methods(registry: MethodRegistry) -> None:
    registry.register("ingestion.local_path", _ingest_local_path)


def _ingest_local_path(params: dict[str, Any]) -> dict[str, Any]:
    """Args: ``{"project_root": "...", "target": "...", "source_id": "..."?}``.

    Returns the persisted manifest dict. Raises :class:`JsonRpcError` for invalid args
    or missing paths.
    """

    project_root_s = params.get("project_root")
    target_s = params.get("target")
    if not isinstance(project_root_s, str) or not project_root_s:
        raise JsonRpcError(error_codes.INVALID_PARAMS, "missing or empty `project_root`")
    if not isinstance(target_s, str) or not target_s:
        raise JsonRpcError(error_codes.INVALID_PARAMS, "missing or empty `target`")

    project_root = Path(project_root_s)
    target = Path(target_s)
    if not project_root.is_dir():
        raise JsonRpcError(
            error_codes.INVALID_PARAMS,
            f"project_root is not a directory: {project_root}",
        )
    if not target.exists():
        raise JsonRpcError(error_codes.INVALID_PARAMS, f"target does not exist: {target}")

    source_id = params.get("source_id")
    if source_id is not None and not isinstance(source_id, str):
        raise JsonRpcError(error_codes.INVALID_PARAMS, "`source_id` must be a string if provided")

    manifest = ingest_local_path(
        project_root=project_root,
        target=target,
        source_id=source_id if isinstance(source_id, str) else None,
    )
    log.info(
        "ingested source %s (%d docs, %d failures)",
        manifest.source_id,
        manifest.document_count,
        manifest.failure_count,
    )
    return manifest.to_dict()
