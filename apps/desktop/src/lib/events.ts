// Strongly-typed event payloads emitted by the Tauri Rust side. The `kind`
// discriminator matches the #[serde(tag = "kind", rename_all = "snake_case")]
// attribute on each enum in the Rust code.

import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type TokenUsage = {
  input_tokens: number;
  output_tokens: number;
};

export type AgentEvent =
  | { kind: "assistant_text_delta"; text: string }
  | { kind: "tool_call_start"; id: string; name: string; input: unknown }
  | {
      kind: "tool_call_end";
      id: string;
      name: string;
      content: string;
      is_error: boolean;
    }
  // `usage` covers one provider iteration only — sum across events for totals.
  | { kind: "turn_end"; reason: StopReason; more_turns: boolean; usage: TokenUsage };

export type StopReason =
  | "end_turn"
  | "max_tokens"
  | "stop_sequence"
  | "tool_use"
  | { error: string };

export type ToolEvent =
  | { kind: "stdout"; data: string }
  | { kind: "stderr"; data: string }
  | { kind: "progress"; data: { message: string } }
  | { kind: "ui_action"; data: UiActionPayload };

export type UiActionPayload =
  | {
      action: "open_chat_preview";
      project: string;
      endpoint: string | null;
      model: string | null;
      filename: string | null;
    }
  | { action: string; [key: string]: unknown };

export function onAgentEvent(handler: (e: AgentEvent) => void): Promise<UnlistenFn> {
  return listen<AgentEvent>("agent:event", (e) => handler(e.payload));
}

export function onToolEvent(handler: (e: ToolEvent) => void): Promise<UnlistenFn> {
  return listen<ToolEvent>("agent:tool", (e) => handler(e.payload));
}

// --- Training (Phase 4) -----------------------------------------------------

/** One metrics.jsonl record / one live training.metric notification. */
export type TrainingMetric = {
  step: number;
  total_steps: number;
  epoch: number;
  loss: number | null;
  eval_loss?: number | null;
  lr: number | null;
  grad_norm: number | null;
  gpu_mem_mb: number;
  eta_secs: number;
  ts?: string;
};

export type TrainingEvent =
  | { kind: "stage"; run_id: string; data: Record<string, unknown> }
  | { kind: "metric"; run_id: string; data: TrainingMetric }
  | { kind: "adapter_answer"; run_id: string; data: Record<string, unknown> }
  | { kind: "finished"; run_id: string; status: string; message: string };

export function onTrainingEvent(handler: (e: TrainingEvent) => void): Promise<UnlistenFn> {
  return listen<TrainingEvent>("training:event", (e) => handler(e.payload));
}

/** Orphaned run from a dead app session (startup scan). User confirms a kill. */
export type OrphanRun = {
  project: string;
  run_id: string;
  pid: number | null;
  pid_alive: boolean;
};

export function onTrainingOrphan(handler: (e: OrphanRun) => void): Promise<UnlistenFn> {
  return listen<OrphanRun>("training:orphan", (e) => handler(e.payload));
}
