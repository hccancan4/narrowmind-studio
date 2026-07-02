// Typed wrappers around the Tauri command surface exposed by apps/desktop/src-tauri.
// Keep these names + arg shapes in sync with the invoke_handler! macro in lib.rs.

import { invoke } from "@tauri-apps/api/core";

export type ProjectSummary = {
  name: string;
  status: string;
  tier: string;
};

export const settings = {
  setProviderKey: (provider: string, apiKey: string) =>
    invoke<void>("set_provider_key", { provider, apiKey }),
  hasProviderKey: (provider: string) =>
    invoke<boolean>("has_provider_key", { provider }),
  deleteProviderKey: (provider: string) =>
    invoke<void>("delete_provider_key", { provider }),
};

export type ProjectProvider = {
  name: string;
  model: string;
  synth_model: string;
};

export const projects = {
  list: () => invoke<ProjectSummary[]>("list_projects"),
  create: (
    name: string,
    tier?: "rag" | "lora" | "hybrid",
    provider?: string,
    model?: string,
  ) => invoke<ProjectSummary>("create_project", { name, tier, provider, model }),
  remove: (name: string) => invoke<void>("delete_project", { name }),
  select: (name: string) => invoke<void>("select_project", { name }),
  current: () => invoke<string | null>("current_project"),
  status: (name: string) => invoke<ProjectSummary>("project_status", { name }),
  /** Read full provider config for one project: agent model + optional cheap synth model. */
  getProvider: (name: string) =>
    invoke<ProjectProvider>("get_project_provider", { name }),
  /** Per-project override for `generate_sft`. Empty string clears the override. */
  setSynthModel: (name: string, synthModel: string) =>
    invoke<void>("set_project_synth_model", { name, synthModel }),
};

export const agent = {
  sendMessage: (message: string) =>
    invoke<void>("agent_send_message", { message }),
  reset: () => invoke<void>("agent_reset"),
  turnCount: () => invoke<number>("agent_turn_count"),
};

// ---------------------------------------------------------------------------
// Chunks (Dataset Browser)
// ---------------------------------------------------------------------------

export type ChunkRecord = {
  chunk_id: string;
  doc_id: string;
  source_id: string;
  text: string;
  token_count: number;
  sentence_range: [number, number];
  include: boolean;
  metadata: Record<string, unknown> | null;
};

export type ListChunksResponse = {
  scanned: number;
  matched: number;
  returned: number;
  offset: number;
  limit: number;
  chunks: ChunkRecord[];
};

export type ListChunksArgs = {
  sourceId?: string;
  search?: string;
  show?: "all" | "included" | "excluded";
  offset?: number;
  limit?: number;
};

export const chunks = {
  list: (args: ListChunksArgs = {}) =>
    invoke<ListChunksResponse>("list_chunks_cmd", {
      sourceId: args.sourceId ?? null,
      search: args.search ?? null,
      show: args.show ?? "all",
      offset: args.offset ?? 0,
      limit: args.limit ?? 200,
    }),
  filter: (sourceId: string, chunkIds: string[], include: boolean) =>
    invoke<{ source_id: string; updated: number; missing: string[]; include: boolean }>(
      "filter_chunks_cmd",
      { sourceId, chunkIds, include },
    ),
};

// ---------------------------------------------------------------------------
// Chat preview
// ---------------------------------------------------------------------------

export type ChatPreviewContext = {
  project: string | null;
  endpoint: string | null;
  model: string | null;
  filename: string | null;
  running: boolean;
  /** Stable key of the live model (`base:<id>` / `domain:<file>`), so the
   *  picker can pre-select it. Null when no server is running. */
  model_key?: string | null;
  /** KARAR 1: set by bootstrap when a training run owns the VRAM. The chat
   *  window is NOT opened; the caller shows an honest status instead. */
  training_active?: boolean;
  run_id?: string;
  step?: number;
  total_steps?: number;
};

/** A model the chat preview can serve: a registry base or an exported domain GGUF. */
export type ServeableModel = {
  key: string;
  label: string;
  kind: "base" | "domain";
};

export type ServeableModels = {
  models: ServeableModel[];
  /** Key of the model currently being served, or null if none. */
  current: string | null;
};

export type ChatHit = {
  chunk_id: string;
  doc_id: string;
  source_id: string;
  text: string;
  token_count: number;
  metadata: Record<string, unknown> | null;
  _distance?: number;
};

// ---------------------------------------------------------------------------
// Training (Phase 4)
// ---------------------------------------------------------------------------

export type TrainingStatus = {
  active: boolean;
  run_id: string | null;
  project: string | null;
  step: number;
  total_steps: number;
  epoch: number;
};

export type TrainingRun = {
  run_id: string;
  status: "running" | "completed" | "failed" | "cancelled" | string;
  base_model?: string;
  started_at?: string;
  finished_at?: string;
  step?: number;
  total_steps?: number;
  epoch?: number;
  best_eval_loss?: number | null;
  error?: string;
  message?: string;
};

export const training = {
  status: () => invoke<TrainingStatus>("training_status"),
  runs: () => invoke<{ runs: TrainingRun[] }>("training_runs"),
  metrics: (runId: string) =>
    invoke<{ run_id: string; metrics: import("./events").TrainingMetric[] }>(
      "training_metrics",
      { runId },
    ),
  logTail: (runId: string, lines = 50) =>
    invoke<{ run_id: string; lines: string[] }>("training_log_tail", { runId, lines }),
  stop: () => invoke<boolean>("training_stop"),
  killOrphan: (project: string, runId: string, pid: number | null) =>
    invoke<boolean>("training_kill_orphan", { project, runId, pid }),
};

export const chat = {
  context: () => invoke<ChatPreviewContext>("chat_preview_context"),
  /** Serveable models for the current project (registry bases + exported domain
   *  GGUFs) plus which one is live — drives the model picker. */
  models: () => invoke<ServeableModels>("chat_preview_models"),
  /**
   * Zero-API entry: ensures a local inference server is running for the selected
   * project and returns the same context shape `context()` does. With no
   * `modelKey` it picks the smart default (the project's domain GGUF if one
   * exists, else the base) and reuses a server already up. Pass a key from
   * `models()` to switch the served model. Throws if no project is selected.
   */
  bootstrap: (modelKey?: string) =>
    invoke<ChatPreviewContext>("chat_preview_bootstrap", { modelKey }),
  send: (
    query: string,
    opts: { topK?: number; maxTokens?: number; temperature?: number } = {},
  ) =>
    invoke<void>("chat_preview_send", {
      query,
      topK: opts.topK ?? 5,
      maxTokens: opts.maxTokens ?? 1024,
      temperature: opts.temperature ?? 0.7,
    }),
};
