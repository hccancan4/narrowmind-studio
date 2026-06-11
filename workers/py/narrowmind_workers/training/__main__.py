"""Training worker entry point. Spawned as ``python -m narrowmind_workers.training``
from the workers/py-training CUDA environment."""

from narrowmind_workers.debug import register_debug_methods
from narrowmind_workers.rpc import MethodRegistry, serve_stdio
from narrowmind_workers.training.rpc import register_methods


def main() -> None:
    registry = MethodRegistry()
    register_methods(registry)
    register_debug_methods(registry)  # no-op unless NM_DEBUG_RPC=1
    serve_stdio(registry)


if __name__ == "__main__":
    main()
