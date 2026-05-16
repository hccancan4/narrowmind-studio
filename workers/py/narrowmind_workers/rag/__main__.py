"""RAG worker entry point. Spawned as ``python -m narrowmind_workers.rag``."""

from narrowmind_workers.rag.rpc import register_methods
from narrowmind_workers.rpc import MethodRegistry, serve_stdio


def main() -> None:
    registry = MethodRegistry()
    register_methods(registry)
    serve_stdio(registry)


if __name__ == "__main__":
    main()
