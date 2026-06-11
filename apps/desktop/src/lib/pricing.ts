// Cost estimation for agent-loop API usage shown in the banner + transcript.
//
// Rates are USD per 1M tokens and deliberately match the orchestrator's
// synth_gen defaults (DEFAULT_INPUT_USD_PER_MTOK / DEFAULT_OUTPUT_USD_PER_MTOK
// in crates/orchestrator/src/tools/synth_gen/mod.rs) — both approximate
// Claude Sonnet pricing. This is an ESTIMATE for cost awareness, not billing:
// per-model rate tables land with multi-provider support in Phase 7.

import type { TokenUsage } from "./events";

export const INPUT_USD_PER_MTOK = 3.0;
export const OUTPUT_USD_PER_MTOK = 15.0;

export function estimateUsd(usage: TokenUsage): number {
  return (
    (usage.input_tokens / 1_000_000) * INPUT_USD_PER_MTOK +
    (usage.output_tokens / 1_000_000) * OUTPUT_USD_PER_MTOK
  );
}

/** 950 -> "950", 1_234 -> "1.2k", 1_234_567 -> "1.2M" */
export function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return `${n}`;
}

/** "$0.0091" below a cent, "$0.09" below a dollar, "$1.23" above. */
export function formatUsd(usd: number): string {
  if (usd === 0) return "$0";
  if (usd < 0.01) return `$${usd.toFixed(4)}`;
  return `$${usd.toFixed(2)}`;
}
