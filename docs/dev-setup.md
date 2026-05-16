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

## Kaspersky and the Claude Code sandbox

On the reference machine **Kaspersky Anti-Virus has blocked process spawning** by `powershell.exe` and `bash.exe` mid-session during heavy install activity. Symptom: every tool call returns `EPERM: operation not permitted, uv_spawn ...` with no other context. Disabling Kaspersky (or whitelisting the relevant executables) restores normal operation. This is environmental; nothing in the repo can fix it.
