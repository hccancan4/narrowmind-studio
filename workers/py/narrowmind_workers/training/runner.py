"""Unsloth QLoRA training run — the heavy, CUDA-only half of the worker.

Everything torch/unsloth is imported INSIDE functions: this module must stay
importable in the CPU environment so rpc.py can register handlers and the
pure pieces stay testable. The first heavy import happens only when
``training.run`` actually executes (inside workers/py-training's venv).

Run artifacts (filesystem is the source of truth — AGENTS principle 3):
  runs/<run_id>/
    status.json       # {run_id, status, step, total_steps, epoch, ...}
    metrics.jsonl     # one line per logging step — durable metric record
    worker.pid        # this process, for orphan detection after app death
    train.log         # human-readable progress (tail shown in UI)
    checkpoints/      # HF Trainer save_strategy=epoch output
    adapter/          # final (best) LoRA adapter, HF format
"""

from __future__ import annotations

import json
import logging
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

from narrowmind_workers.rpc import notify
from narrowmind_workers.training.config import TrainingParams
from narrowmind_workers.training.dataset import (
    load_sft_pairs,
    split_train_validation,
    to_messages,
    truncation_prescreen,
)

log = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# LoRA target-module resolution (pure, testable)
# ---------------------------------------------------------------------------

# The linear projections LoRA adapts on Qwen2.5 / Llama / Gemma family models:
# the four attention projections + the three MLP projections. This is the
# canonical "all linear layers" set Unsloth's own examples target.
ALL_LINEAR_MODULES = [
    "q_proj",
    "k_proj",
    "v_proj",
    "o_proj",
    "gate_proj",
    "up_proj",
    "down_proj",
]


def resolve_target_modules(spec: str) -> list[str]:
    """Resolve the config ``target_modules`` string to an explicit module list.

    The installed Unsloth/PEFT build does NOT accept the ``"all-linear"`` string
    keyword: passed as-is it iterates the string into a set of single characters
    (``{'a', 'l', '-', 'i', 'n', 'e', 'r'}``) and fails the run with
    "Target modules ... not found in the base model". Always hand it a list.

    - ``"all-linear"`` -> the canonical 7 linear projections.
    - anything else -> comma-split into a list, so even a bare ``"q_proj"``
      becomes ``["q_proj"]`` rather than a set of characters.
    """
    if spec == "all-linear":
        return list(ALL_LINEAR_MODULES)
    return [m.strip() for m in spec.split(",") if m.strip()]


# ---------------------------------------------------------------------------
# Domain-GGUF export path helpers (pure, testable) — Phase 4.6 slice 3
# ---------------------------------------------------------------------------


def slugify_domain(name: str) -> str:
    """Filesystem-safe domain slug for a GGUF filename. Lowercase, [a-z0-9-_],
    other runs collapsed to a single '-'."""
    out: list[str] = []
    for ch in name.lower():
        out.append(ch if (ch.isalnum() or ch in "-_") else "-")
    slug = "".join(out).strip("-")
    while "--" in slug:
        slug = slug.replace("--", "-")
    return slug or "domain"


def domain_gguf_path(project_root: Path, domain: str, quant: str) -> Path:
    """Where a domain GGUF lands: ``<project>/models/<slug>-<quant>.gguf``.
    One GGUF per domain, managed as files (Phase 4.6 — replaces runtime
    multi-LoRA paging)."""
    return project_root / "models" / f"{slugify_domain(domain)}-{quant.lower()}.gguf"


def llama_cpp_dir() -> Path:
    """Root of the vendored llama.cpp build (convert script + binaries). Set
    ``LLAMA_CPP_DIR`` in run-worker.sh; defaults to ``~/llama.cpp`` (built once
    in WSL — see docs/dev-setup.md)."""
    env = os.environ.get("LLAMA_CPP_DIR")
    return Path(env) if env else (Path.home() / "llama.cpp")


# ---------------------------------------------------------------------------
# Run-directory bookkeeping (pure, testable)
# ---------------------------------------------------------------------------


def run_dir(project_root: Path, run_id: str) -> Path:
    return project_root / "runs" / run_id


def write_status(rd: Path, **fields: Any) -> None:
    """Merge-update status.json atomically (tmp + replace)."""
    path = rd / "status.json"
    current: dict[str, Any] = {}
    if path.is_file():
        try:
            current = json.loads(path.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError):
            current = {}
    current.update(fields)
    tmp = path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(current, ensure_ascii=False, indent=2), encoding="utf-8")
    tmp.replace(path)


def append_metric(rd: Path, record: dict[str, Any]) -> None:
    with (rd / "metrics.jsonl").open("a", encoding="utf-8") as f:
        f.write(json.dumps(record, ensure_ascii=False) + "\n")


def append_log(rd: Path, msg: str) -> None:
    with (rd / "train.log").open("a", encoding="utf-8") as f:
        f.write(f"[{time.strftime('%H:%M:%S')}] {msg}\n")


# ---------------------------------------------------------------------------
# The training run (CUDA env only past this point)
# ---------------------------------------------------------------------------


def execute_training(
    project_root: Path,
    run_id: str,
    params: TrainingParams,
    resume_from: str | None,
) -> dict[str, Any]:
    """Blocking QLoRA run. Streams ``training.metric`` notifications per
    logging step AND appends the same record to metrics.jsonl — the file is
    the durable record, the notification is the live-UI optimization."""
    rd = run_dir(project_root, run_id)
    rd.mkdir(parents=True, exist_ok=True)
    (rd / "checkpoints").mkdir(exist_ok=True)
    (rd / "worker.pid").write_text(str(os.getpid()), encoding="utf-8")
    write_status(
        rd,
        run_id=run_id,
        status="running",
        base_model=params.base_model_hf_repo,
        started_at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        step=0,
        total_steps=0,
        epoch=0,
    )
    append_log(rd, f"run {run_id} starting: {params.base_model_hf_repo}")

    # --- Dataset (pure) ----------------------------------------------------
    sft_path = project_root / "datasets" / "sft.jsonl"
    pairs = load_sft_pairs(sft_path)
    if len(pairs) < 10:
        raise ValueError(
            f"sft.jsonl has only {len(pairs)} usable pairs (need >= 10); run generate_sft first"
        )
    train_pairs, val_pairs = split_train_validation(
        pairs, params.validation_split, params.seed
    )
    prescreen = truncation_prescreen(pairs, params.max_seq_length)
    notify("training.stage", {"stage": "dataset", **prescreen,
                              "train": len(train_pairs), "validation": len(val_pairs)})
    append_log(
        rd,
        f"dataset: {len(train_pairs)} train + {len(val_pairs)} validation pairs; "
        f"~{prescreen['estimated_over_budget']} may exceed seq budget (char estimate)",
    )

    # --- Heavy imports (CUDA env) -------------------------------------------
    notify("training.stage", {"stage": "loading_model", "repo": params.base_model_hf_repo})
    append_log(rd, "importing unsloth + loading 4-bit base model (first run downloads ~5 GB)")
    from unsloth import FastLanguageModel  # noqa: PLC0415 — deliberate lazy import

    import torch  # noqa: PLC0415

    model, tokenizer = FastLanguageModel.from_pretrained(
        model_name=params.base_model_hf_repo,
        max_seq_length=params.max_seq_length,
        load_in_4bit=params.load_in_4bit,
        dtype=None,  # auto: bf16 on Ampere
    )
    model = FastLanguageModel.get_peft_model(
        model,
        r=params.lora_r,
        lora_alpha=params.lora_alpha,
        # Explicit list, never the "all-linear" string — see resolve_target_modules.
        target_modules=resolve_target_modules(params.target_modules),
        use_gradient_checkpointing=params.gradient_checkpointing,
        random_state=params.seed,
    )

    # --- Real-tokenizer truncation stats (KARAR 7) ---------------------------
    def render(pair: dict[str, str]) -> str:
        return tokenizer.apply_chat_template(
            to_messages(pair), tokenize=False, add_generation_prompt=False
        )

    over = sum(
        1
        for p in pairs
        if len(tokenizer(render(p), truncation=False)["input_ids"]) > params.max_seq_length
    )
    notify("training.stage", {"stage": "tokenized", "truncated_pairs": over, "total": len(pairs)})
    append_log(rd, f"tokenizer truncation: {over}/{len(pairs)} pairs exceed {params.max_seq_length} tokens")

    # --- HF datasets ----------------------------------------------------------
    from datasets import Dataset  # noqa: PLC0415

    train_ds = Dataset.from_list([{"text": render(p)} for p in train_pairs])
    val_ds = Dataset.from_list([{"text": render(p)} for p in val_pairs])

    # --- Trainer --------------------------------------------------------------
    from transformers import TrainerCallback  # noqa: PLC0415
    from trl import SFTConfig, SFTTrainer  # noqa: PLC0415

    sft_config = SFTConfig(
        output_dir=str(rd / "checkpoints"),
        per_device_train_batch_size=params.per_device_train_batch_size,
        gradient_accumulation_steps=params.gradient_accumulation_steps,
        num_train_epochs=params.num_train_epochs,
        learning_rate=params.learning_rate,
        warmup_ratio=params.warmup_ratio,
        lr_scheduler_type=params.lr_scheduler_type,
        bf16=params.bf16,
        optim=params.optim,
        seed=params.seed,
        logging_steps=1,
        save_strategy="epoch",
        save_total_limit=params.save_total_limit,
        eval_strategy="epoch",
        load_best_model_at_end=True,
        metric_for_best_model="eval_loss",
        greater_is_better=False,
        max_seq_length=params.max_seq_length,
        dataset_text_field="text",
        report_to=[],  # no wandb/tensorboard — metrics.jsonl is our record
    )

    run_start = time.time()

    class MetricStream(TrainerCallback):
        """Per-step: notify (live UI) + metrics.jsonl (durable) + status.json."""

        def on_log(self, args: Any, state: Any, control: Any, logs: dict | None = None, **kw: Any) -> None:
            if not logs:
                return
            gpu_mem_mb = 0
            if torch.cuda.is_available():
                gpu_mem_mb = int(torch.cuda.memory_allocated() / (1024 * 1024))
            step = int(state.global_step)
            total = int(state.max_steps) if state.max_steps else 0
            elapsed = time.time() - run_start
            eta = int(elapsed / step * (total - step)) if step > 0 and total > 0 else 0
            record = {
                "step": step,
                "total_steps": total,
                "epoch": round(float(state.epoch or 0.0), 3),
                "loss": logs.get("loss"),
                "eval_loss": logs.get("eval_loss"),
                "lr": logs.get("learning_rate"),
                "grad_norm": logs.get("grad_norm"),
                "gpu_mem_mb": gpu_mem_mb,
                "eta_secs": eta,
                "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            }
            append_metric(rd, record)
            notify("training.metric", record)
            write_status(rd, step=step, total_steps=total, epoch=record["epoch"])
            if logs.get("loss") is not None:
                append_log(rd, f"step {step}/{total} loss={logs['loss']:.4f}")

    trainer = SFTTrainer(
        model=model,
        processing_class=tokenizer,
        train_dataset=train_ds,
        eval_dataset=val_ds,
        args=sft_config,
        callbacks=[MetricStream()],
    )

    notify("training.stage", {"stage": "training"})
    if resume_from:
        append_log(rd, f"resuming from checkpoint dir of run {resume_from}")
        resume_dir = _latest_checkpoint(run_dir(project_root, resume_from) / "checkpoints")
        trainer.train(resume_from_checkpoint=str(resume_dir) if resume_dir else None)
    else:
        trainer.train()

    # --- Save final (best) adapter -------------------------------------------
    adapter_dir = rd / "adapter"
    model.save_pretrained(str(adapter_dir))
    tokenizer.save_pretrained(str(adapter_dir))
    append_log(rd, f"adapter saved to {adapter_dir}")

    best = getattr(trainer.state, "best_metric", None)
    write_status(
        rd,
        status="completed",
        finished_at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        best_eval_loss=best,
        adapter_dir=str(adapter_dir),
    )
    return {
        "run_id": run_id,
        "status": "completed",
        "adapter_dir": str(adapter_dir),
        "best_eval_loss": best,
        "train_pairs": len(train_pairs),
        "validation_pairs": len(val_pairs),
        "truncated_pairs": over,
    }


def _latest_checkpoint(checkpoints_dir: Path) -> Path | None:
    """HF Trainer writes checkpoint-<step> dirs; pick the highest step."""
    if not checkpoints_dir.is_dir():
        return None
    best: tuple[int, Path] | None = None
    for entry in checkpoints_dir.iterdir():
        if entry.is_dir() and entry.name.startswith("checkpoint-"):
            try:
                step = int(entry.name.split("-", 1)[1])
            except ValueError:
                continue
            if best is None or step > best[0]:
                best = (step, entry)
    return best[1] if best else None


# ---------------------------------------------------------------------------
# Adapter smoke test (M6 — KARAR 8: HF format only, no GGUF here)
# ---------------------------------------------------------------------------


def execute_test_adapter(
    project_root: Path,
    run_id: str,
    questions: list[str],
    max_new_tokens: int,
) -> dict[str, Any]:
    """Load base(4bit) + adapter, answer a handful of questions. A verification
    aid, not a serving path — slow is fine. VRAM mutex is the caller's job."""
    rd = run_dir(project_root, run_id)
    adapter_dir = rd / "adapter"
    if not adapter_dir.is_dir():
        raise ValueError(f"no adapter at {adapter_dir} — run training first")

    status = json.loads((rd / "status.json").read_text(encoding="utf-8"))
    base_repo = status.get("base_model", "")
    if not base_repo:
        raise ValueError("status.json missing base_model")

    notify("training.stage", {"stage": "loading_adapter", "adapter": str(adapter_dir)})
    from unsloth import FastLanguageModel  # noqa: PLC0415

    model, tokenizer = FastLanguageModel.from_pretrained(
        model_name=str(adapter_dir),  # unsloth resolves adapter + base from config
        max_seq_length=2048,
        load_in_4bit=True,
        dtype=None,
    )
    FastLanguageModel.for_inference(model)

    answers: list[dict[str, str]] = []
    for i, q in enumerate(questions):
        rendered = tokenizer.apply_chat_template(
            [{"role": "user", "content": q}],
            tokenize=False,
            add_generation_prompt=True,
        )
        inputs = tokenizer(rendered, return_tensors="pt").to(model.device)
        output = model.generate(
            **inputs,
            max_new_tokens=max_new_tokens,
            do_sample=False,
            temperature=None,
            top_p=None,
        )
        text = tokenizer.decode(
            output[0][inputs["input_ids"].shape[1]:], skip_special_tokens=True
        )
        answers.append({"question": q, "answer": text.strip()})
        notify("training.adapter_answer", {"index": i, "of": len(questions), "question": q})

    return {"run_id": run_id, "answers": answers}


# ---------------------------------------------------------------------------
# Domain-GGUF export (Phase 4.6 slice 3): merge adapter -> GGUF -> quantize
# ---------------------------------------------------------------------------


def _run_export_cmd(
    cmd: list[str], *, cwd: Path | None = None, extra_pythonpath: str | None = None
) -> None:
    """Run a llama.cpp subprocess (convert / quantize). On non-zero exit, raise
    RuntimeError with the last 15 stderr lines so the failure is actionable."""
    env = dict(os.environ)
    if extra_pythonpath:
        env["PYTHONPATH"] = extra_pythonpath + os.pathsep + env.get("PYTHONPATH", "")
    proc = subprocess.run(  # noqa: S603 — fixed argv, no shell
        cmd,
        cwd=str(cwd) if cwd else None,
        env=env,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )
    if proc.returncode != 0:
        tail = "\n".join((proc.stderr or proc.stdout or "").splitlines()[-15:])
        raise RuntimeError(f"`{Path(cmd[0]).name}` exited {proc.returncode}:\n{tail}")


def execute_export_gguf(
    project_root: Path,
    run_id: str,
    quant: str = "Q4_K_M",
    domain: str | None = None,
) -> dict[str, Any]:
    """Merge the run's LoRA adapter into its base and produce ONE quantized
    domain GGUF the llama.cpp inference server can load.

    Stages (heavy intermediates on WSL ext4 staging, never the OneDrive project
    dir):
      1. Unsloth ``save_pretrained_merged(merged_16bit)``: adapter+base -> 16-bit HF.
      2. ``convert_hf_to_gguf.py`` -> f16 GGUF.
      3. ``llama-quantize`` -> <quant> -> ``project/models/<slug>-<quant>.gguf``.

    Slice 4 swaps step 3 for a domain-imatrix-calibrated quant. The VRAM mutex
    (stop the inference server first) is the caller's job, like training."""
    rd = run_dir(project_root, run_id)
    adapter_dir = rd / "adapter"
    if not adapter_dir.is_dir():
        raise ValueError(f"no adapter at {adapter_dir} — run training first")
    domain = domain or project_root.name

    llama = llama_cpp_dir()
    convert_py = llama / "convert_hf_to_gguf.py"
    quantize_bin = llama / "build" / "bin" / "llama-quantize"
    gguf_py = llama / "gguf-py"
    for p in (convert_py, quantize_bin):
        if not p.exists():
            raise ValueError(
                f"llama.cpp artifact missing: {p}. Build it once in WSL "
                "(docs/dev-setup.md) or set LLAMA_CPP_DIR."
            )

    staging_root = Path(
        os.environ.get("NM_EXPORT_STAGING", str(Path.home() / ".cache" / "narrowmind-export"))
    )
    stage = staging_root / run_id
    merged_dir = stage / "merged"
    f16_gguf = stage / "f16.gguf"
    out = domain_gguf_path(project_root, domain, quant)
    out.parent.mkdir(parents=True, exist_ok=True)
    merged_dir.mkdir(parents=True, exist_ok=True)

    try:
        # 1) merge adapter into base -> 16-bit HF (CPU-offloaded on an 8 GB card).
        notify("export.stage", {"stage": "merging", "adapter": str(adapter_dir)})
        append_log(rd, f"export: merging adapter -> 16-bit HF ({merged_dir})")
        from unsloth import FastLanguageModel  # noqa: PLC0415

        model, tokenizer = FastLanguageModel.from_pretrained(
            model_name=str(adapter_dir), max_seq_length=2048, load_in_4bit=True, dtype=None
        )
        model.save_pretrained_merged(str(merged_dir), tokenizer, save_method="merged_16bit")

        # 2) HF -> f16 GGUF
        notify("export.stage", {"stage": "converting"})
        append_log(rd, "export: convert_hf_to_gguf -> f16 GGUF")
        _run_export_cmd(
            [
                sys.executable, str(convert_py), str(merged_dir),
                "--outfile", str(f16_gguf), "--outtype", "f16",
            ],
            cwd=llama,
            extra_pythonpath=str(gguf_py),
        )

        # 3) quantize -> project models/
        notify("export.stage", {"stage": "quantizing", "quant": quant})
        append_log(rd, f"export: quantize {quant} -> {out}")
        _run_export_cmd([str(quantize_bin), str(f16_gguf), str(out), quant])

        size_mb = out.stat().st_size // (1024 * 1024)
        append_log(rd, f"export: done -> {out} ({size_mb} MB)")
        notify("export.stage", {"stage": "done", "path": str(out), "size_mb": size_mb})
        return {
            "run_id": run_id,
            "domain": slugify_domain(domain),
            "quant": quant,
            "gguf_path": str(out),
            "size_mb": size_mb,
        }
    finally:
        shutil.rmtree(stage, ignore_errors=True)  # reclaim ~30 GB of intermediates
