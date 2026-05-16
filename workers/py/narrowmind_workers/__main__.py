"""Module entry point. Spawned by the Rust orchestrator as `python -m narrowmind_workers`."""

from narrowmind_workers.hello import hello
from narrowmind_workers.rpc import MethodRegistry, serve_stdio


def main() -> None:
    registry = MethodRegistry()
    registry.register("hello", hello)
    serve_stdio(registry)


if __name__ == "__main__":
    main()
