# Development Setup

Reference machine: **RTX 3070 (8 GB VRAM) + 32 GB RAM**, **Windows 11 + WSL Ubuntu**. The repo is designed to build on macOS and Linux too, but this document covers the Windows-specific quirks the reference machine hit during Phase 0 bootstrap.

For the general install steps see [README.md](../README.md). This file documents *why* certain pieces exist, not what to type.

---

## `.nm-env.ps1` — local dev env helper

A gitignored PowerShell script at the repo root that prepares the current shell for `cargo`, `pnpm`, `uv`, `gh`, and the MSVC build environment. Dot-source it before any build command:

```powershell
. .\.nm-env.ps1
```

It does three things:

1. **Prepends toolchain bin directories to `$env:Path`.** winget on this machine installs pnpm, uv, and GitHub CLI into the `WinGet\Packages\<id>\` sandbox, which is *not* added to PATH for child processes spawned by tools like Claude Code. Hardcoded fallback paths keep every shell repeatable instead of depending on the parent process having refreshed its environment after the install.

2. **Locates the MSVC build environment via `vswhere`.** Uses the standard Visual Studio Installer locator (`vswhere -all -products *`). The `-latest` flag intentionally is not used: see the next section.

3. **Imports the MSVC environment via `vcvarsx86_amd64.bat`.** Spawns `cmd /c "<vcvars> >nul 2>&1 && set"`, parses the output, and copies every variable into the current PowerShell session. After this `cl.exe`, `link.exe`, `INCLUDE`, `LIB`, and the Windows SDK paths are all wired up for the rest of the session.

The script is **gitignored on purpose** — it hardcodes paths that are specific to this machine. The intent is to copy it to your own Windows dev box and adjust as needed; we will replace it with a proper detection script once we hit the first contributor whose paths differ in load-bearing ways.

---

## MSVC build workaround for Build Tools 2026 (v18 preview)

The reference machine ships with **Visual Studio Build Tools 2026 (v18.4.11626.88)**, which at the time of Phase 0 bootstrap is a preview release. Two compatibility gaps had to be papered over before `cargo build` worked at all:

### Gap 1 — `vcvars64.bat` is missing

The standard `VC\Auxiliary\Build\vcvars64.bat` is not shipped in this Build Tools install. Only the cross-compile variants exist:

```
VC\Auxiliary\Build\vcvars32.bat
VC\Auxiliary\Build\vcvarsall.bat
VC\Auxiliary\Build\vcvarsx86_amd64.bat
```

`vcvarsall.bat x64` runs but does not append the MSVC `bin\HostX64\x64` directory to `PATH` — likely because only the **HostX86** toolchain (`bin\HostX86\x64`) is installed. The cross-compile script `vcvarsx86_amd64.bat` is the one that correctly wires up the HostX86 cross to the x64 target. The env helper falls back through this priority order:

1. `vcvars64.bat` (preferred — native x64 host)
2. `vcvarsx86_amd64.bat` (cross — what this machine actually uses)
3. `vcvarsall.bat x64` (last resort)

A native Hostx64 install would let us use option 1; switching toolchains is a Phase 7 polish item.

### Gap 2 — `msvcrt.lib` only exists in `lib\onecore\x64`

The MSVC linker invocation rust-lld targets resolves to `/defaultlib:msvcrt`, which requires `msvcrt.lib` somewhere on `LIB`. In this Build Tools install the standard `VC\Tools\MSVC\<ver>\lib\x64` directory **does not contain `msvcrt.lib`** — only the OneCore variant under `lib\onecore\x64\msvcrt.lib` exists.

After the vcvars import the env helper appends `VC\Tools\MSVC\<latest>\lib\onecore\x64` to `LIB` so the linker can resolve it. The OneCore CRT exports the same symbols Rust binaries need for desktop builds; we have not yet hit a case where this matters semantically, but if Tauri's webview build starts pulling in classic Win32 APIs that OneCore omits, the right fix is to install the standard Desktop development with C++ workload via the Visual Studio Installer rather than to keep extending the workaround.

### Symptom checklist

If a Rust build on Windows fails with one of these errors, the corresponding piece of the env helper is what's missing:

| Error | Missing piece |
|---|---|
| `linker `link.exe` not found` | MSVC `bin\HostX*\x64` not on PATH → vcvars never ran |
| `LNK1104: cannot open file 'kernel32.lib'` | Windows SDK not on `LIB` → `winget install Microsoft.WindowsSDK.10.0.22621` |
| `LNK1104: cannot open file 'msvcrt.lib'` | onecore fallback not appended to `LIB` → see gap 2 above |

---

## Recovering from a stale env

PowerShell does not re-read PATH from the registry mid-session, so `winget install`s done in one terminal are invisible to terminals already open. If a previously-working command suddenly says "not recognized," dot-source `.nm-env.ps1` again — or open a new terminal.

---

## CUDA Toolkit for GPU inference (Phase 3)

Phase 3 adds a local llama.cpp inference server via `llama-cpp-python`. On Windows the install pulls a **cu125 prebuilt wheel** directly from abetlen's GitHub releases (pinned in `workers/py/pyproject.toml`) because abetlen does not publish cu125 to a PEP503 index. The wheel's `llama.dll` dynamically links `cudart64_12.dll` + `cublas64_12.dll`, so **CUDA Toolkit 12.5 must be installed system-wide** for inference to work.

### Why 12.5 specifically

The reference machine had **CUDA Toolkit 13.2** already installed, but the abetlen prebuilt is compiled against the 12.x runtime ABI — loading `llama.dll` against 13.x DLLs fails with `cudart64_12.dll could not be found`. Two options:

- **Side-by-side install of 12.5** (what this machine does): download from <https://developer.nvidia.com/cuda-12-5-1-download-archive>. The installer is happy to coexist with 13.2 and uses a separate `v12.5\` directory.
- **Build the wheel from source against 13.x**: skip the prebuilt URL in `pyproject.toml` and let `uv` build from PyPI. Requires the full CUDA Toolkit + a matching MSVC toolchain — usually slower and more error-prone than the side-by-side approach.

### `.nm-env.ps1` prepends `CUDA\v12.5\bin`

Parent processes that started before the CUDA Toolkit install (e.g. the IDE host) carry a stale `PATH` and cannot find `cudart64_12.dll` even though it's on the registry-level system PATH. The env helper hardcodes the prepend so every dot-source is self-sufficient:

```powershell
$env:Path = "...;${env:ProgramFiles}\NVIDIA GPU Computing Toolkit\CUDA\v12.5\bin;..."
```

### Verifying GPU offload

After dot-sourcing, this should print `True` and list the GPU:

```powershell
. .\.nm-env.ps1
uv --directory workers/py run python -c "from llama_cpp import llama_supports_gpu_offload; print(llama_supports_gpu_offload())"
# expected: ggml_cuda_init: found 1 CUDA devices ... NVIDIA GeForce RTX 3070
# True
```

In the orchestrator log during a real inference run, `load_tensors: layer N assigned to device CUDA0` for every layer confirms full offload. If you see `assigned to device CPU` for all 28 layers, the wheel is the CPU fallback — re-check the install order.

---

## Training environment (Phase 4): WSL2 + separate uv project

Phase 4's QLoRA training runs on **Unsloth**, whose wheels mark native
Windows unsupported (every dependency carries a `sys_platform != 'win32'`
marker as of unsloth 2026.5.x). The training environment therefore lives in
**WSL2 Ubuntu**, where NVIDIA's driver passes CUDA through (verify with
`wsl -e nvidia-smi` — it should list the host GPU).

Two further constraints shaped the layout:

1. **CPU-torch vs CUDA-torch**: the main uv workspace pins torch to the CPU
   index (BGE-small needs no GPU). A uv workspace shares one lockfile and one
   `.venv`, so the CUDA build cannot coexist there. `workers/py-training/` is
   a **standalone uv project** (excluded from the workspace in the root
   pyproject) with its own lock: torch (>=2.6, from the `cu126` index — unsloth
   2026.x needs `torch.int1` dtypes that landed in 2.6) + unsloth + bitsandbytes
   + trl/peft/accelerate.
2. **No path-dependency on narrowmind-workers**: a path dep would drag the
   CPU-torch source pin into the training resolution and conflict. Instead the
   worker is spawned with `PYTHONPATH=<repo>/workers/py` — sound because the
   training entry's import graph outside the ML stack is pure stdlib
   (rpc/server.py, debug.py, training/config.py, training/dataset.py; heavy
   imports are deferred into handler bodies).

### One-time setup

```bash
# inside WSL (Ubuntu)
curl -LsSf https://astral.sh/uv/install.sh | sh
```

Then from Windows, the initial sync (downloads ~3-5 GB of CUDA wheels):

```powershell
wsl -e bash -c 'export PATH="$HOME/.local/bin:$PATH" && export UV_PROJECT_ENVIRONMENT="$HOME/.venvs/narrowmind-training" && cd "/mnt/c/<repo>/workers/py-training" && uv sync'
```

### Why the venv and HF cache live on ext4, not /mnt/c

`run-worker.sh` (the launcher the orchestrator spawns through `wsl.exe -e
bash ./run-worker.sh ...`) sets:

- `UV_PROJECT_ENVIRONMENT=$HOME/.venvs/narrowmind-training` — importing torch
  from a venv on the 9p-mounted NTFS costs 30-60 s of file-open round-trips;
  on ext4 it's ~2 s. The `uv.lock` stays in the repo.
- `HF_HOME=$HOME/.cache/narrowmind-hf` — memory-mapping a ~5 GB safetensors
  base model over 9p is brutal. The training base model is therefore cached
  separately from the Windows-side GGUF cache (one-time disk cost, paid for
  load speed every run).

Run artifacts (`runs/<id>/metrics.jsonl`, `status.json`, checkpoints, the
final adapter) DO live on the Windows side under the project directory —
the Rust orchestrator and the Training Monitor read them natively; epoch-level
checkpoint writes over 9p are slow but rare (3 per run).

### Verifying the training environment

```powershell
wsl -e bash "workers/py-training/run-worker.sh" -c "import torch; print(torch.cuda.is_available(), torch.cuda.get_device_name(0))"
# expected: True NVIDIA GeForce RTX 3070
```

### Orphan-process note

`worker.pid` written by a training run is a **Linux pid in the WSL
namespace** — Windows `tasklist` cannot see it. Orphan detection therefore
probes via `wsl -e kill -0 <pid>` and kills via `wsl -e kill -9 <pid>`.

### `NARROWMIND_PROJECTS_ROOT` override

The training worker reads the project directory THROUGH `/mnt/c/...`, i.e.
the real host filesystem. If your projects directory is not host-visible at
its default location (sandboxed dev sessions are the known case — file
writes to `%APPDATA%` may land in an overlay WSL can't see), set
`NARROWMIND_PROJECTS_ROOT` before launching the app to point at a directory
both sides can read — the repo's gitignored `projects/` folder works:

```powershell
$env:NARROWMIND_PROJECTS_ROOT = "<repo>\projects"
pnpm --filter @narrowmind/desktop tauri dev
```

---

## llama.cpp build for GGUF export (Phase 4.6)

The produce→GGUF path (merge adapter → GGUF → quantize, and the domain-imatrix
quant in slice 4) needs llama.cpp's `convert_hf_to_gguf.py` + `llama-quantize`
+ `llama-imatrix`. These are **not** in the `llama-cpp-python` wheel, so we
build them once in WSL (CPU build is enough — quantize/imatrix don't need CUDA;
imatrix over a small domain corpus runs fine on CPU):

```bash
# inside WSL — cmake via uv avoids a sudo apt install
uv tool install cmake
git clone --depth 1 https://github.com/ggml-org/llama.cpp ~/llama.cpp
cd ~/llama.cpp
cmake -B build -DGGML_CUDA=OFF -DLLAMA_CURL=OFF -DCMAKE_BUILD_TYPE=Release
cmake --build build -j"$(nproc)" --target llama-quantize llama-imatrix
```

The export worker finds these via `LLAMA_CPP_DIR` (defaults to `~/llama.cpp`).
`convert_hf_to_gguf.py` runs under the py-training interpreter with `PYTHONPATH`
pointed at `~/llama.cpp/gguf-py`.

### Why intermediates live on ext4, not the project dir

A 7B LoRA merged to 16-bit HF is ~15 GB, and the f16 GGUF another ~15 GB. The
export stages both under `~/.cache/narrowmind-export/<run_id>/` (ext4) and
deletes them when done — the project dir is under OneDrive, and 30 GB of
transient blobs there would be brutal to sync. Only the final quantized GGUF
(~4.4 GB at Q4_K_M) lands at `projects/<name>/models/<slug>-<quant>.gguf`.

---

## Worker stdio is strict UTF-8

The Python workers communicate with the Rust orchestrator over JSON-RPC on stdin/stdout. **Windows defaults `sys.stdout` encoding to the system locale (cp1252)**, not UTF-8. Wikipedia chunks contain em dashes, smart quotes, and accented letters; writing them with `json.dumps(ensure_ascii=False)` against a cp1252 stream produces single-byte values like `0x97` that the Rust `BufReader::read_line` rejects as `stream did not contain valid UTF-8`.

`narrowmind_workers.rpc.serve_stdio` reconfigures stdout/stderr to UTF-8 before serving and also redirects `sys.stdout` to `sys.stderr` so third-party noise (sentence-transformers' `Loading weights:` progress bar, stray `print()` calls) cannot poison the protocol stream. **Any new worker that calls `serve_stdio` inherits this for free.** Workers that need a custom serve loop must replicate the encoding setup or the next chunk with non-ASCII text will break the call.

---

## Kaspersky and the Claude Code sandbox

On the reference machine **Kaspersky Anti-Virus has blocked process spawning** by `powershell.exe` and `bash.exe` mid-session during heavy install activity. Symptom: every tool call returns `EPERM: operation not permitted, uv_spawn ...` with no other context. Disabling Kaspersky (or whitelisting the relevant executables) restores normal operation. This is environmental; nothing in the repo can fix it.
