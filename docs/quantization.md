# Quantization — PTQ, QAT, and the Gemma 4 Trap

Why this document exists: the model registry (`crates/orchestrator/src/models.rs`)
carries per-model `quantization_notes` that warn users away from specific
mistakes. This page is the long-form background those notes compress. It also
pins down what Phase 6's export pipeline will and will not produce.

---

## PTQ vs QAT

**Post-Training Quantization (PTQ)** compresses a model *after* training
finished. The weights were learned in bf16/fp16; a conversion pass maps them
to 4-bit (or other) blocks, choosing scales that minimize rounding error —
often guided by an importance matrix ("imatrix") computed over calibration
text. The model never knew it would be quantized; quality loss is whatever
the rounding costs.

- **Our Qwen2.5-7B baseline is PTQ**: `bartowski/Qwen2.5-7B-Instruct-GGUF`'s
  Q4_K_M is a llama.cpp imatrix conversion. Proven in Phase 3/3.5 evals
  (recall 0.98 / judge 4.55 on the 56-pair set) — for this model family the
  PTQ quality tax at Q4_K_M is acceptable and well-understood.

**Quantization-Aware Training (QAT)** simulates quantization *during*
training: forward passes run against fake-quantized weights, so the optimizer
learns weight values that survive rounding. The released checkpoint is
designed to be served quantized. Done right, a QAT 4-bit model recovers most
of the quality gap to its bf16 parent — that's why Google's Gemma 4 QAT
checkpoints (released 2026-06-05) cut VRAM ~3× with near-original quality.

**The asymmetry that matters:** QAT is a *training-time* property. You can
PTQ any checkpoint you have; you cannot retrofit QAT onto one. Whoever trains
the model decides whether QAT exists.

---

## The Gemma 4 naive-conversion trap

QAT checkpoints are not "regular weights, but better at quantizing." They
encode quantization parameters with specific assumptions — Gemma 4's QAT
weights were trained against **BF16 scales**, while llama.cpp's stock Q4_0
conversion uses **F16 scales** and picks them by its own heuristic. A naive
`convert_hf_to_gguf.py` + `quantize Q4_0` run misinterprets the very thing
QAT optimized.

Measured damage (community benchmarks at release, June 2026):

| Model | QAT-aware GGUF | Naive Q4_0 conversion | Loss |
|---|---:|---:|---:|
| Gemma 4 26B-A4B | 85.6 % | 70.2 % | **−15.4 pts** |
| Gemma 4 31B | 96.7 % | 87.9 % | **−8.8 pts** |

Unsloth's analysis put naive conversion at only ~25 % byte-exactness against
the true QAT weights. Their "dynamic" QAT-aware GGUFs recover the loss.

**Deployment rule for this codebase** (enforced by a registry test that
requires the Gemma `gguf_repo` to stay QAT-flavored):

> Gemma 4 models are served ONLY from QAT-aware GGUF sources
> (`unsloth/gemma-4-12B-it-qat-GGUF` for the 12B). Never run a generic
> llama.cpp conversion against Gemma 4 QAT checkpoints, and never "simplify"
> the registry entry to a generic GGUF repo.

The Qwen rule is the opposite and that's fine: Qwen has no QAT checkpoints,
PTQ imatrix conversions are the normal, well-tested path.

---

## What Phase 6 export will produce

Phase 6 ships "export trained DSLM → GGUF → Ollama Modelfile." Scope pin:

- **Our export is PTQ.** After a LoRA merge, we quantize the merged weights
  with llama.cpp's standard pipeline (Q4_K_M default, imatrix optional).
  That is the right tool: our fine-tunes start from instruct checkpoints and
  train adapters for minutes-to-hours; nobody is re-running pretraining.
- **We do not produce QAT.** QAT is a training-time regime owned by the base
  model vendor (Google, for Gemma). If a user fine-tunes a QAT base, the
  merged-and-PTQ'd export loses the QAT guarantee — the export UI should say
  so when the base is a QAT model (registry `quantization_notes` is the
  source for that warning).
- Practical consequence for Gemma 4 fine-tunes: serve the LoRA *adapter*
  alongside the QAT base GGUF where possible (llama.cpp `--lora`), rather
  than merge-and-requantize, to keep the QAT weights intact. Decision point
  lands in Phase 6 design.

---

## Quick reference

| | Qwen2.5-7B (default) | Gemma 4 12B |
|---|---|---|
| Quant regime | PTQ (imatrix Q4_K_M) | Official QAT checkpoints |
| Safe GGUF source | any reputable PTQ conversion (we pin bartowski) | QAT-aware ONLY (we pin Unsloth) |
| Naive conversion risk | normal PTQ tax | −8.8…−15.4 pts — never do it |
| VRAM @ recommended quant | ~5 GB | ~6.6 GB |
| Fine-tune (QLoRA, Unsloth) | supported, comfortable on 8 GB | supported, 8-10 GB — at the 3070 floor |
| Turkish quality | eval'd informally in Phase 3 (dogfood corpus is EN) | unverified — eval before Turkish production use |
