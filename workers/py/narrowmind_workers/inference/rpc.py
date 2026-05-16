"""GGUF download RPC. The orchestrator calls this once before starting the server.

Idempotent: HF Hub's local cache is consulted first — already-downloaded files return
their path immediately without re-checking the remote.
"""

from __future__ import annotations

import logging
from typing import Any

from narrowmind_workers.rpc import JsonRpcError, MethodRegistry, error_codes

log = logging.getLogger(__name__)


def register_methods(registry: MethodRegistry) -> None:
    registry.register("inference.download_model", _download_model)


def _download_model(params: dict[str, Any]) -> dict[str, Any]:
    """Args: ``{"repo_id": "bartowski/Qwen2.5-7B-Instruct-GGUF", "filename": "...Q4_K_M.gguf"}``.

    Returns: ``{"path": "<absolute>", "size_bytes": N, "cached": bool}``.
    """

    repo_id = params.get("repo_id")
    filename = params.get("filename")
    if not isinstance(repo_id, str) or not repo_id:
        raise JsonRpcError(error_codes.INVALID_PARAMS, "missing or empty `repo_id`")
    if not isinstance(filename, str) or not filename:
        raise JsonRpcError(error_codes.INVALID_PARAMS, "missing or empty `filename`")

    # Local import: huggingface_hub is a transitive dep of sentence-transformers, but
    # keeping it out of module-top means tests that don't touch this RPC don't pay the
    # import cost.
    from huggingface_hub import hf_hub_download, try_to_load_from_cache
    from pathlib import Path

    cached = try_to_load_from_cache(repo_id=repo_id, filename=filename)
    if isinstance(cached, str):
        path = cached
        was_cached = True
    else:
        log.info("downloading %s :: %s (may take several minutes)", repo_id, filename)
        path = hf_hub_download(repo_id=repo_id, filename=filename)
        was_cached = False

    size_bytes = Path(path).stat().st_size
    log.info("model ready at %s (%.2f GB, cached=%s)", path, size_bytes / 1e9, was_cached)
    return {"path": path, "size_bytes": size_bytes, "cached": was_cached}
