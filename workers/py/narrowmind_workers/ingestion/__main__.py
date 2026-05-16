"""Ingestion worker entry point.

Spawned by the Rust orchestrator as ``python -m narrowmind_workers.ingestion``.
"""

from narrowmind_workers.ingestion.rpc import register_methods
from narrowmind_workers.rpc import MethodRegistry, serve_stdio


def main() -> None:
    registry = MethodRegistry()
    register_methods(registry)
    serve_stdio(registry)


if __name__ == "__main__":
    main()
