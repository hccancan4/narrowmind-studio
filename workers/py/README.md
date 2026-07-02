# narrowmind-workers

Python ML workers for NarrowMind Studio. Each worker is a long-lived process speaking JSON-RPC 2.0 over stdio. Workers are crash-isolated from the Rust orchestrator (subprocess boundary) and stateless between requests — all project state lives on disk.

## Workers (planned)

| Module | Phase | Purpose |
|---|---|---|
| `rpc` | 0 | JSON-RPC 2.0 stdio server runtime shared by every worker |
| `ingestion` | 2 | PDF / EPUB / DOCX / web / Wikipedia / HF datasets ingest |
| `training` | 4 | Unsloth QLoRA fine-tuning (Axolotl optional) |
| `inference` | 3 | llama.cpp server wrapper, OpenAI-compatible local endpoint |
| `rag` | 3 | LanceDB + BGE-small (sentence-transformers) + BM25/RRF hybrid retrieval |
| `eval` | 5 | LLM-judge scoring lives Rust-side in `run_eval` (`tools/eval.rs`); manual rating UI planned |
| `export` | 6 | GGUF conversion, quantization, Ollama Modelfile generation |

Phase 0 ships only the `hello` RPC method to prove the orchestrator ↔ worker round-trip works.
