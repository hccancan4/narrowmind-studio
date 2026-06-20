"""JSON-RPC entry point exposed by the SFT-import worker.

Method names are the contract the Rust orchestrator (`import_sft_from_hf` tool) calls.
Keep the args + return shape stable — bump method names rather than change semantics.
"""

from __future__ import annotations

import logging
from typing import Any

from narrowmind_workers.rpc import JsonRpcError, MethodRegistry, error_codes
from narrowmind_workers.sft_import.loader import import_hf_qa

log = logging.getLogger(__name__)


def register_methods(registry: MethodRegistry) -> None:
    registry.register("sft_import.import_hf_qa", _import_hf_qa)


def _import_hf_qa(params: dict[str, Any]) -> dict[str, Any]:
    """Args: ``{"sources": [{"repo_id", ...}], "seed": int, "max_total": int?}``.

    Returns ``{"pairs": [...], "meta": {...}}``. Raises :class:`JsonRpcError` on bad args.
    """

    sources = params.get("sources")
    if not isinstance(sources, list) or not sources:
        raise JsonRpcError(error_codes.INVALID_PARAMS, "`sources` must be a non-empty list")
    for src in sources:
        if not isinstance(src, dict) or not isinstance(src.get("repo_id"), str) or not src["repo_id"]:
            raise JsonRpcError(error_codes.INVALID_PARAMS, "each source needs a non-empty `repo_id`")
        cats = src.get("categories")
        if cats is not None and not (isinstance(cats, list) and all(isinstance(c, str) for c in cats)):
            raise JsonRpcError(error_codes.INVALID_PARAMS, "`categories` must be a list of strings")
        if cats and not src.get("category_col"):
            raise JsonRpcError(error_codes.INVALID_PARAMS, "`categories` requires `category_col`")

    seed = params.get("seed", 0)
    if not isinstance(seed, int) or isinstance(seed, bool):
        raise JsonRpcError(error_codes.INVALID_PARAMS, "`seed` must be an integer")

    max_total = params.get("max_total")
    if max_total is not None and (
        not isinstance(max_total, int) or isinstance(max_total, bool) or max_total < 1
    ):
        raise JsonRpcError(error_codes.INVALID_PARAMS, "`max_total` must be a positive integer")

    result = import_hf_qa(sources=sources, seed=seed, max_total=max_total)
    log.info("sft_import: %d pairs from %d source(s)", result["meta"]["total"], len(sources))
    return result
