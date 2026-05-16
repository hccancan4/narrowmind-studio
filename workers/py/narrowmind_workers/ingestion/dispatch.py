"""Extension → handler dispatch for local file ingestion."""

from __future__ import annotations

import logging
from collections.abc import Callable
from pathlib import Path
from typing import Any

from narrowmind_workers.ingestion.handlers import docx, epub, html, pdf, text

log = logging.getLogger(__name__)

# Returns (title, text, metadata) or raises.
Handler = Callable[[Path], tuple[str, str, dict[str, Any]]]

# Conservative: only formats we have explicit handlers for. Anything else is skipped
# with a recorded failure so the user knows ingestion didn't silently drop their file.
EXTENSION_HANDLERS: dict[str, Handler] = {
    ".txt": text.extract,
    ".md": text.extract,
    ".markdown": text.extract,
    ".rst": text.extract,
    ".pdf": pdf.extract,
    ".docx": docx.extract,
    ".epub": epub.extract,
    ".html": html.extract,
    ".htm": html.extract,
}


def supported_extensions() -> list[str]:
    return sorted(EXTENSION_HANDLERS.keys())


def handler_for(path: Path) -> Handler | None:
    return EXTENSION_HANDLERS.get(path.suffix.lower())
