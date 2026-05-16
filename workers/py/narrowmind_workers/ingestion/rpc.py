"""JSON-RPC entry points exposed by the ingestion worker.

Method names are the contract the Rust orchestrator calls. Keep the args + return shape
stable — bump method names rather than silently change semantics.
"""

from __future__ import annotations

import logging
from pathlib import Path
from typing import Any

from narrowmind_workers.ingestion.local import ingest_local_path
from narrowmind_workers.ingestion.url import ingest_url
from narrowmind_workers.ingestion.wikipedia import ingest_wikipedia_category
from narrowmind_workers.rpc import JsonRpcError, MethodRegistry, error_codes

log = logging.getLogger(__name__)


def register_methods(registry: MethodRegistry) -> None:
    registry.register("ingestion.local_path", _ingest_local_path)
    registry.register("ingestion.wikipedia", _ingest_wikipedia)
    registry.register("ingestion.url", _ingest_url)


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


def _ingest_wikipedia(params: dict[str, Any]) -> dict[str, Any]:
    """Args: ``{"project_root", "category", "language"?, "max_depth"?, "max_pages"?, "source_id"?}``."""

    project_root = _required_dir(params, "project_root")
    category = _required_str(params, "category")
    language = params.get("language", "en")
    if not isinstance(language, str) or not language:
        raise JsonRpcError(error_codes.INVALID_PARAMS, "`language` must be a non-empty string")
    max_depth = _bounded_int(params, "max_depth", default=1, minimum=0, maximum=5)
    max_pages = _bounded_int(params, "max_pages", default=50, minimum=1, maximum=2000)
    source_id = params.get("source_id") if isinstance(params.get("source_id"), str) else None

    manifest = ingest_wikipedia_category(
        project_root=project_root,
        category=category,
        language=language,
        max_depth=max_depth,
        max_pages=max_pages,
        source_id=source_id,
    )
    log.info(
        "wikipedia source %s (%d docs, %d failures)",
        manifest.source_id,
        manifest.document_count,
        manifest.failure_count,
    )
    return manifest.to_dict()


def _ingest_url(params: dict[str, Any]) -> dict[str, Any]:
    """Args: ``{"project_root", "url", "max_depth"?, "max_pages"?, "same_domain_only"?, "source_id"?}``."""

    project_root = _required_dir(params, "project_root")
    url = _required_str(params, "url")
    max_depth = _bounded_int(params, "max_depth", default=0, minimum=0, maximum=3)
    max_pages = _bounded_int(params, "max_pages", default=1, minimum=1, maximum=200)
    same_domain_only = bool(params.get("same_domain_only", True))
    source_id = params.get("source_id") if isinstance(params.get("source_id"), str) else None

    manifest = ingest_url(
        project_root=project_root,
        url=url,
        max_depth=max_depth,
        max_pages=max_pages,
        same_domain_only=same_domain_only,
        source_id=source_id,
    )
    log.info(
        "url source %s (%d docs, %d failures)",
        manifest.source_id,
        manifest.document_count,
        manifest.failure_count,
    )
    return manifest.to_dict()


# ---------------------------------------------------------------------------
# Param helpers
# ---------------------------------------------------------------------------


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


def _bounded_int(params: dict[str, Any], key: str, default: int, minimum: int, maximum: int) -> int:
    v = params.get(key, default)
    if not isinstance(v, int) or isinstance(v, bool):
        raise JsonRpcError(error_codes.INVALID_PARAMS, f"`{key}` must be an integer")
    if v < minimum or v > maximum:
        raise JsonRpcError(
            error_codes.INVALID_PARAMS, f"`{key}` must be in [{minimum}, {maximum}], got {v}"
        )
    return v
