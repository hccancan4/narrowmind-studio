"""Per-format text extraction handlers.

Each handler exports an ``extract(path)`` function returning ``(title, text, metadata)``
or raises an exception. The dispatcher routes by file extension.
"""
