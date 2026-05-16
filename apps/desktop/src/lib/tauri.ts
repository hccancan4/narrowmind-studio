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
