"""HuggingFace QA dataset → SFT import worker (Phase 5).

Pulls ready-made ``question`` / ``answer`` pairs from HF datasets directly into a
project's ``datasets/sft.jsonl``, bypassing synthetic generation. See
:func:`loader.select_pairs` for the pure (network-free) filtering/sampling logic the
tests exercise.
"""

from narrowmind_workers.sft_import.loader import import_hf_qa, select_pairs

__all__ = ["import_hf_qa", "select_pairs"]
