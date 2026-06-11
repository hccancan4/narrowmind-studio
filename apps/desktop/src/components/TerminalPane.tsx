import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

import { onAgentEvent, onToolEvent, type StopReason, type TokenUsage } from "../lib/events";
import { estimateUsd, formatTokens, formatUsd } from "../lib/pricing";

const ANSI = {
  reset: "\x1b[0m",
  dim: "\x1b[2m",
  red: "\x1b[31m",
  green: "\x1b[32m",
  yellow: "\x1b[33m",
  cyan: "\x1b[36m",
  gray: "\x1b[90m",
};

const ANSI_RE = /\x1b\[[0-9;]*m/g;

function visibleLen(s: string): number {
  return s.replace(ANSI_RE, "").length;
}

/**
 * Soft-wrap a single logical line to fit inside `cols` visible columns. xterm
 * will hard-break at the column boundary mid-character if we hand it a line
 * that's too long — which during Phase 3 acceptance produced URLs and JSON
 * payloads sliced at awkward points like `bartowski/Qwen2.5-7B-Instruct-GGUF/`
 * and `"temperature":0.3,"`. This wraps at the last whitespace, `/`, or `,`
 * before the boundary so the break points are at least readable. Continuation
 * lines get `indent` prepended so the eye can track them.
 *
 * ANSI escape codes are preserved verbatim and don't count toward the width.
 */
function softWrap(line: string, cols: number, indent = "  "): string[] {
  if (cols <= 0 || visibleLen(line) <= cols) return [line];
  const out: string[] = [];
  let remaining = line;
  let isContinuation = false;
  while (remaining.length > 0) {
    const width = isContinuation ? Math.max(20, cols - indent.length) : cols;
    if (visibleLen(remaining) <= width) {
      out.push(isContinuation ? indent + remaining : remaining);
      break;
    }
    // Find a soft break: walk from the budget backwards looking for whitespace
    // or a path/URL separator that we'd rather break on than mid-token.
    let breakAt = -1;
    let visible = 0;
    let lastSoft = -1;
    for (let i = 0; i < remaining.length && visible <= width; i++) {
      const ch = remaining[i] as string;
      // Skip past full ANSI escape sequences without counting them.
      if (ch === "\x1b" && remaining[i + 1] === "[") {
        const end = remaining.indexOf("m", i);
        if (end !== -1) {
          i = end;
          continue;
        }
      }
      visible++;
      if (visible <= width && /[\s/,]/.test(ch)) lastSoft = i;
      if (visible === width) breakAt = i;
    }
    const splitAt = lastSoft > 0 ? lastSoft + 1 : breakAt > 0 ? breakAt + 1 : remaining.length;
    out.push((isContinuation ? indent : "") + remaining.slice(0, splitAt).trimEnd());
    remaining = remaining.slice(splitAt).replace(/^[ \t]+/, "");
    isContinuation = true;
  }
  return out;
}

function formatStop(reason: StopReason): string {
  if (typeof reason === "string") return reason;
  if ("error" in reason) return `error: ${reason.error}`;
  return JSON.stringify(reason);
}

/** " · 1.2k in / 350 out · ~$0.0089" or "" when the provider reported nothing. */
function formatUsageSuffix(usage: TokenUsage | undefined): string {
  if (!usage || (usage.input_tokens === 0 && usage.output_tokens === 0)) return "";
  const tokens = `${formatTokens(usage.input_tokens)} in / ${formatTokens(usage.output_tokens)} out`;
  return ` · ${tokens} · ~${formatUsd(estimateUsd(usage))}`;
}

export type TerminalHandle = {
  writeUserPrompt: (text: string) => void;
  writeError: (text: string) => void;
  clear: () => void;
};

type Props = {
  onReady?: (handle: TerminalHandle) => void;
};

export function TerminalPane({ onReady }: Props) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);

  useEffect(() => {
    if (!hostRef.current) return;
    const term = new Terminal({
      convertEol: true,
      cursorBlink: false,
      cursorStyle: "bar",
      disableStdin: true,
      fontFamily:
        'ui-monospace, "Cascadia Code", "JetBrains Mono", "Source Code Pro", monospace',
      fontSize: 13,
      lineHeight: 1.2,
      scrollback: 5000,
      theme: {
        background: "#0a0a0e",
        foreground: "#dcdcdc",
        cursor: "#dcdcdc",
        black: "#1f1f23",
        red: "#ef6b6b",
        green: "#86c270",
        yellow: "#e9c46a",
        blue: "#6cb6ff",
        magenta: "#c792ea",
        cyan: "#5eb9d0",
        white: "#dcdcdc",
        brightBlack: "#5a5a60",
      },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(hostRef.current);
    setTimeout(() => fit.fit(), 0);
    termRef.current = term;
    fitRef.current = fit;

    // Helper: write a line softly wrapped to the current terminal width. We
    // can't rely on xterm's hard wrap because it slices URLs/JSON mid-token.
    // Subtract 1 from cols so the right edge has a one-cell breathing room
    // and the cursor never lands in the "wrap-but-also-newline" ambiguous spot.
    const writeWrapped = (line: string) => {
      const cols = Math.max(20, (term.cols || 80) - 1);
      for (const piece of softWrap(line, cols)) {
        term.writeln(piece);
      }
    };

    term.writeln(`${ANSI.gray}NarrowMind Studio — Phase 1 shell${ANSI.reset}`);
    term.writeln(`${ANSI.gray}select a project on the left, then type below to talk to the agent${ANSI.reset}`);
    term.writeln("");

    // Refit on every layout change to the host element, not just window resize.
    // The center pane shrinks when the right sidebar grows and a plain
    // window.resize listener misses that. But we also have to be careful: a
    // naive ResizeObserver -> fit.fit() pair fires on EVERY layout invalidation
    // including the ones fit.fit() itself causes (term.resize changes the
    // canvas height which can shift adjacent siblings). That tight loop
    // swallowed wheel events during streaming — Phase 3 user could no longer
    // scroll up in the terminal. Fixes:
    //   1. Only refit when the host's content box actually changed since last
    //      fit. Avoids spurious calls from cosmetic layout updates.
    //   2. rAF-coalesce successive RO callbacks so we fit at most once per
    //      frame even when streaming text triggers many invalidations.
    let lastW = 0;
    let lastH = 0;
    let pending = 0;
    const refit = () => {
      pending = 0;
      const host = hostRef.current;
      if (!host) return;
      const w = host.clientWidth;
      const h = host.clientHeight;
      if (w === lastW && h === lastH) return;
      lastW = w;
      lastH = h;
      try {
        fit.fit();
      } catch {
        /* host may be detached mid-tick */
      }
    };
    const scheduleRefit = () => {
      if (pending) return;
      pending = requestAnimationFrame(refit);
    };
    window.addEventListener("resize", scheduleRefit);
    const ro = new ResizeObserver(scheduleRefit);
    ro.observe(hostRef.current);

    onReady?.({
      writeUserPrompt: (text) => {
        writeWrapped(`${ANSI.cyan}> ${text}${ANSI.reset}`);
      },
      writeError: (text) => {
        writeWrapped(`${ANSI.red}[error] ${text}${ANSI.reset}`);
      },
      clear: () => term.clear(),
    });

    // Per-request usage accumulator: every TurnEnd carries only its own
    // iteration's tokens, so we sum across iterations and print the request
    // total on the final TurnEnd, then reset for the next user message.
    let requestUsage = { input_tokens: 0, output_tokens: 0 };

    const unlistenAgent = onAgentEvent((ev) => {
      switch (ev.kind) {
        case "assistant_text_delta":
          // Streaming tokens don't end with a newline, so we can't soft-wrap
          // per-chunk without losing the streaming feel. Let xterm hard-wrap
          // these — they're prose, not URLs, so mid-word wrap is acceptable.
          term.write(ev.text);
          break;
        case "tool_call_start":
          term.writeln("");
          writeWrapped(
            `${ANSI.yellow}→ tool ${ev.name}${ANSI.reset}  ${ANSI.gray}${JSON.stringify(ev.input)}${ANSI.reset}`,
          );
          break;
        case "tool_call_end": {
          const color = ev.is_error ? ANSI.red : ANSI.green;
          const label = ev.is_error ? "error" : "ok";
          writeWrapped(`${color}← ${ev.name} (${label})${ANSI.reset}`);
          for (const line of ev.content.split("\n")) {
            writeWrapped(`  ${ANSI.dim}${line}${ANSI.reset}`);
          }
          break;
        }
        case "turn_end": {
          if (ev.usage) {
            requestUsage.input_tokens += ev.usage.input_tokens;
            requestUsage.output_tokens += ev.usage.output_tokens;
          }
          if (!ev.more_turns) {
            term.writeln("");
            writeWrapped(
              `${ANSI.gray}─── turn complete (${formatStop(ev.reason)}${formatUsageSuffix(requestUsage)}) ───${ANSI.reset}`,
            );
            term.writeln("");
            requestUsage = { input_tokens: 0, output_tokens: 0 };
          }
          break;
        }
      }
    });
    const unlistenTool = onToolEvent((ev) => {
      switch (ev.kind) {
        case "stdout":
          writeWrapped(`${ANSI.dim}${ev.data}${ANSI.reset}`);
          break;
        case "stderr":
          writeWrapped(`${ANSI.red}${ev.data}${ANSI.reset}`);
          break;
        case "progress":
          writeWrapped(`${ANSI.cyan}… ${ev.data.message}${ANSI.reset}`);
          break;
        case "ui_action":
          // The actual open-chat-preview wiring lives on App.tsx, which subscribes to
          // the same event channel. Here we just leave a breadcrumb in the transcript.
          writeWrapped(`${ANSI.cyan}… ui action: ${ev.data.action}${ANSI.reset}`);
          break;
      }
    });

    return () => {
      window.removeEventListener("resize", scheduleRefit);
      if (pending) cancelAnimationFrame(pending);
      ro.disconnect();
      unlistenAgent.then((u) => u()).catch(() => {});
      unlistenTool.then((u) => u()).catch(() => {});
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
    };
  }, [onReady]);

  return <div className="terminal" ref={hostRef} />;
}
