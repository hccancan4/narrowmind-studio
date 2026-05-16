"""RAG worker: BGE-small embedding + LanceDB vector store + retrieval queries."""

from narrowmind_workers.rag.embedder import DIM, MODEL_ID, embed
from narrowmind_workers.rag.index import (
    TABLE_NAME,
    count_rows,
    open_db,
    query,
    store_path,
    upsert_chunks,
)
from narrowmind_workers.rag.rpc import register_methods

__all__ = [
    "DIM",
    "MODEL_ID",
    "TABLE_NAME",
    "count_rows",
    "embed",
    "open_db",
    "query",
    "register_methods",
    "store_path",
    "upsert_chunks",
]
