#!/usr/bin/env bash
# NarrowMind training-worker launcher — runs INSIDE WSL2.
#
# Why this script exists: unsloth's wheels mark native Windows unsupported
# (all deps carry `sys_platform != 'win32'` markers), so the Phase 4 training
# environment lives in WSL2 Ubuntu where CUDA passes through the Windows
# driver. The Rust orchestrator spawns:
#
#     wsl.exe -e bash ./run-worker.sh -m narrowmind_workers.training
#
# with the Windows cwd set to workers/py-training (wsl.exe auto-translates
# the working directory to /mnt/c/...). Everything after the script name is
# forwarded to `uv run python` verbatim.
#
# Environment decisions, each load-bearing:
# - UV_PROJECT_ENVIRONMENT: the venv lives on WSL-native ext4, NOT /mnt/c.
#   Importing torch from a 9p-mounted venv costs 30-60 s of file-open
#   round-trips; on ext4 it's ~2 s. The uv.lock stays in the repo on NTFS.
# - PYTHONPATH: the worker CODE stays in the repo (workers/py). The training
#   env deliberately has no path-dependency on narrowmind-workers (it would
#   drag the CPU-torch source pin along) — the stdlib-only rpc core imports
#   fine straight off the source tree.
# - HF_HOME: model weights cache on WSL-native ext4 too — memory-mapping a
#   5 GB safetensors file over 9p is brutal. This means the training base
#   model is cached SEPARATELY from the Windows-side GGUF cache; one-time
#   ~5 GB cost, paid for load speed every run.
set -euo pipefail

export PATH="$HOME/.local/bin:$PATH"
export UV_PROJECT_ENVIRONMENT="$HOME/.venvs/narrowmind-training"
export HF_HOME="$HOME/.cache/narrowmind-hf"
export HF_HUB_DISABLE_PROGRESS_BARS=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export PYTHONPATH="${SCRIPT_DIR}/../py"

cd "${SCRIPT_DIR}"
exec uv run python "$@"
