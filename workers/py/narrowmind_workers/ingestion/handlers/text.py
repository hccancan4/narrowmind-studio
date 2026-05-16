"""Plain text and markdown handler.

We treat ``.md`` like ``.txt`` for ingestion: markdown markup is preserved as-is so the
chunker can split on headings / list items. Downstream cleaning may strip it if needed —
that's not this handler's job.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

# Detect encodings from common BOMs before falling back to UTF-8 with lossy decode.
_ENCODINGS = ["utf-8", "utf-16", "latin-1"]


def extract(path: Path) -> tuple[str, str, dict[str, Any]]:
    raw = path.read_bytes()
    text = ""
    encoding_used = "utf-8"
    for enc in _ENCODINGS:
        try:
            text = raw.decode(enc)
            encoding_used = enc
            break
        except UnicodeDecodeError:
            continue
    if not text:
        text = raw.decode("utf-8", errors="replace")
        encoding_used = "utf-8-lossy"

    # Title heuristic: first non-empty line, capped at 120 chars.
    title = path.stem
    for line in text.splitlines():
        stripped = line.lstrip("# ").strip()
        if stripped:
            title = stripped[:120]
            break

    return title, text, {"encoding": encoding_used, "format": path.suffix.lstrip(".").lower() or "txt"}
