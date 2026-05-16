"""BGE-small-en-v1.5 embedder, lazy-loaded.

Lazy because:
- ``sentence-transformers`` + torch is a ~5 s import that touches GBs of RAM.
- Phase 2 ingestion + cleaning + chunking paths don't need embeddings — only Phase 3's
  ``build_dataset`` triggers this. The first call downloads ~130 MB into HF_HOME (which
  the orchestrator points at ``{APP_DATA_DIR}/narrowmind/hf-cache/`` per Phase 2 decision C).

Vectors are L2-normalised so a dot product gives cosine similarity directly — convenient
for LanceDB's metric calculation.
"""

from __future__ import annotations

import logging
from functools import lru_cache

log = logging.getLogger(__name__)

MODEL_ID = "BAAI/bge-small-en-v1.5"
DIM = 384


@lru_cache(maxsize=1)
def _model():
    """Lazy single-instance model. Imports sentence-transformers on first call."""
    log.info("loading %s (lazy first call, ~5 s + initial download if absent)", MODEL_ID)
    # Import here to keep module import cheap when only the type / constants are needed.
    from sentence_transformers import SentenceTransformer

    return SentenceTransformer(MODEL_ID, device="cpu")


def embed(texts: list[str], batch_size: int = 64) -> list[list[float]]:
    """Embed a batch of texts into 384-dim L2-normalised vectors.

    Empty input returns an empty list without touching the model.
    """
    if not texts:
        return []
    model = _model()
    vectors = model.encode(
        texts,
        batch_size=batch_size,
        show_progress_bar=False,
        normalize_embeddings=True,
        convert_to_numpy=True,
    )
    return vectors.tolist()


def embed_one(text: str) -> list[float]:
    """Convenience: embed a single string."""
    return embed([text])[0]
