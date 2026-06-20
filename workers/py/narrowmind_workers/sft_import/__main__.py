"""SFT-import worker entry point.

Spawned by the Rust orchestrator as ``python -m narrowmind_workers.sft_import``.
"""

from narrowmind_workers.rpc import MethodRegistry, serve_stdio
from narrowmind_workers.sft_import.rpc import register_methods


def main() -> None:
    registry = MethodRegistry()
    register_methods(registry)
    serve_stdio(registry)


if __name__ == "__main__":
    main()
