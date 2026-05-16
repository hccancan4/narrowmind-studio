"""Inference worker entry point. Used by the orchestrator for the download RPC.
The HTTP server itself runs as ``python -m llama_cpp.server`` and is managed by Rust."""

from narrowmind_workers.inference.rpc import register_methods
from narrowmind_workers.rpc import MethodRegistry, serve_stdio


def main() -> None:
    registry = MethodRegistry()
    register_methods(registry)
    serve_stdio(registry)


if __name__ == "__main__":
    main()
