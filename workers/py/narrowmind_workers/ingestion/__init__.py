"""Ingestion worker — extracts text from local files, URLs, Wikipedia, and HF datasets.

Public surface is the JSON-RPC handler registry; everything else is internal.
See :mod:`narrowmind_workers.ingestion.rpc` for the method list.
"""

from narrowmind_workers.ingestion.models import Document, SourceManifest, SourceType
from narrowmind_workers.ingestion.rpc import register_methods

__all__ = ["Document", "SourceManifest", "SourceType", "register_methods"]
