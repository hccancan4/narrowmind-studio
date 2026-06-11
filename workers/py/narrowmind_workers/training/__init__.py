"""Training worker (Phase 4) — Unsloth QLoRA fine-tuning with streaming metrics.

Runs ONLY in the CUDA environment (workers/py-training); the heavy imports
(unsloth, torch) are deferred into handler bodies so the pure pieces
(config validation, dataset formatting) stay unit-testable in the CPU env.
"""

from narrowmind_workers.training.rpc import register_methods

__all__ = ["register_methods"]
