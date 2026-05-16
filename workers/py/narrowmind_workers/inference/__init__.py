"""Inference helpers: GGUF model download.

The actual HTTP serving is done by ``llama_cpp.server`` started directly by the Rust
orchestrator (long-lived process, OpenAI-compatible endpoint). This module exposes a
single RPC for resolving + downloading a GGUF file from a HuggingFace repo since
``huggingface_hub`` is already in our Python deps and handles partial-resume + cache."""

from narrowmind_workers.inference.rpc import register_methods

__all__ = ["register_methods"]
