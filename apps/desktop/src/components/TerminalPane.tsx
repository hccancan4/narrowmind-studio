import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

import { onAgentEvent, onToolEvent, type StopReason } from "../lib/events";

const ANSI = {
  reset: "\x1b[0m",
  dim: "\x1b[2m",
  red: "\x1b[31m",
  green: "\x1b[32m",
  yellow: "\x1b[33m",
  cyan: "\x1b[36m",
  gray: "\x1b[90m",
};

function formatStop(reason: StopReason): string {
  if (typeof reason === "string") return reason;
  if ("error" in reason) return `error: ${reason.error}`;
  return JSON.stringify(reason);
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
    term.writeln(`${ANSI.gray}NarrowMind Studio — Phase 1 shell${ANSI.reset}`);
    term.writeln(`${ANSI.gray}select a project on the left, then type below to talk to the agent${ANSI.reset}`);
    term.writeln("");

    const resize = () => fit.fit();
    window.addEventListener("resize", resize);

    onReady?.({
      writeUserPrompt: (text) => {
        term.writeln(`${ANSI.cyan}> ${text}${ANSI.reset}`);
      },
      writeError: (text) => {
        term.writeln(`${ANSI.red}[error] ${text}${ANSI.reset}`);
      },
      clear: () => term.clear(),
    });

    const unlistenAgent = onAgentEvent((ev) => {
      switch (ev.kind) {
        case "assistant_text_delta":
          term.write(ev.text);
          break;
        case "tool_call_start":
          term.writeln("");
          term.writeln(
            `${ANSI.yellow}→ tool ${ev.name}${ANSI.reset}  ${ANSI.gray}${JSON.stringify(ev.input)}${ANSI.reset}`,
          );
          break;
        case "tool_call_end": {
          const color = ev.is_error ? ANSI.red : ANSI.green;
          const label = ev.is_error ? "error" : "ok";
          term.writeln(`${color}← ${ev.name} (${label})${ANSI.reset}`);
          for (const line of ev.content.split("\n")) {
            term.writeln(`  ${ANSI.dim}${line}${ANSI.reset}`);
          }
          break;
        }
        case "turn_end": {
          if (!ev.more_turns) {
            term.writeln("");
            term.writeln(`${ANSI.gray}─── turn complete (${formatStop(ev.reason)}) ───${ANSI.reset}`);
            term.writeln("");
          }
          break;
        }
      }
    });
    const unlistenTool = onToolEvent((ev) => {
      switch (ev.kind) {
        case "stdout":
          term.writeln(`${ANSI.dim}${ev.data}${ANSI.reset}`);
          break;
        case "stderr":
          term.writeln(`${ANSI.red}${ev.data}${ANSI.reset}`);
          break;
        case "progress":
          term.writeln(`${ANSI.cyan}… ${ev.data.message}${ANSI.reset}`);
          break;
      }
    });

    return () => {
      window.removeEventListener("resize", resize);
      unlistenAgent.then((u) => u()).catch(() => {});
      unlistenTool.then((u) => u()).catch(() => {});
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
    };
  }, [onReady]);

  return <div className="terminal" ref={hostRef} />;
}
