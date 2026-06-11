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

export const debugCmd = {
  helloRoundTrip: (name?: string) =>
    invoke<{
      message: string;
      worker_version: string;
      worker_pid: number;
      python_version: string;
      platform: string;
    }>("hello_round_trip_cmd", { name }),
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
  /** KARAR 1: set by bootstrap when a training run owns the VRAM. The chat
   *  window is NOT opened; the caller shows an honest status instead. */
  training_active?: boolean;
  run_id?: string;
  step?: number;
  total_steps?: number;
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

export const chat = {
  context: () => invoke<ChatPreviewContext>("chat_preview_context"),
  /**
   * Zero-API entry: ensures the local Qwen inference server is running for the
   * currently-selected project and returns the same context shape `context()`
   * does. The "Local chat" banner button calls this so the floating window can
   * open without first paying for a Sonnet turn just to fire the
   * `open_chat_preview` agent tool. Throws if no project is selected.
   */
  bootstrap: () => invoke<ChatPreviewContext>("chat_preview_bootstrap"),
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
