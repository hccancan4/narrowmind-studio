// Strongly-typed event payloads emitted by the Tauri Rust side. The `kind`
// discriminator matches the #[serde(tag = "kind", rename_all = "snake_case")]
// attribute on each enum in the Rust code.

import { listen, type UnlistenFn } from "@tauri-apps/api/event";

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
  | { kind: "turn_end"; reason: StopReason; more_turns: boolean };

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
