import { useCallback, useEffect, useRef, useState } from "react";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";

import { InputBar } from "./components/InputBar";
import { ProjectRail } from "./components/ProjectRail";
import { RightSidebar } from "./components/RightSidebar";
import { SettingsDialog } from "./components/SettingsDialog";
import { TerminalPane, type TerminalHandle } from "./components/TerminalPane";
import { onAgentEvent, onToolEvent, onTrainingOrphan, type TokenUsage } from "./lib/events";
import { estimateUsd, formatTokens, formatUsd } from "./lib/pricing";
import { agent, chat, settings, training } from "./lib/tauri";

export function App() {
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [selectedProject, setSelectedProject] = useState<string | null>(null);
  const [hasKey, setHasKey] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [localChatBusy, setLocalChatBusy] = useState(false);
  // Cumulative agent-loop API usage for this app session. Local Chat never
  // moves this counter — that's the whole point of the zero-API path.
  const [sessionUsage, setSessionUsage] = useState<TokenUsage>({
    input_tokens: 0,
    output_tokens: 0,
  });
  const termRef = useRef<TerminalHandle | null>(null);

  const onTerminalReady = useCallback((handle: TerminalHandle) => {
    termRef.current = handle;
  }, []);

  useEffect(() => {
    settings.hasProviderKey("anthropic").then(setHasKey).catch(() => setHasKey(false));
  }, [settingsOpen]);

  // Accumulate per-iteration usage from every TurnEnd into the session total
  // shown in the banner. Each event carries only its own stream's tokens
  // (AgentEvent contract), so plain addition never double-counts.
  useEffect(() => {
    const u = onAgentEvent((ev) => {
      if (ev.kind !== "turn_end" || !ev.usage) return;
      const { input_tokens, output_tokens } = ev.usage;
      if (input_tokens === 0 && output_tokens === 0) return;
      setSessionUsage((prev) => ({
        input_tokens: prev.input_tokens + input_tokens,
        output_tokens: prev.output_tokens + output_tokens,
      }));
    });
    return () => {
      u.then((un) => un()).catch(() => {});
    };
  }, []);

  // Orphan training processes from a dead app session (M5): the startup scan
  // emits one event per live orphan; the user confirms before anything dies.
  useEffect(() => {
    const u = onTrainingOrphan(async (o) => {
      const ok = window.confirm(
        `Orphan training process detected (project ${o.project}, run ${o.run_id.slice(0, 8)}, WSL pid ${o.pid ?? "?"}) — it may still be using the GPU.\n\nKill it?`,
      );
      if (!ok) return;
      try {
        await training.killOrphan(o.project, o.run_id, o.pid);
        termRef.current?.writeError(
          `orphan training process (pid ${o.pid ?? "?"}) killed; run ${o.run_id.slice(0, 8)} marked failed`,
        );
      } catch (e) {
        termRef.current?.writeError(`orphan kill failed: ${e}`);
      }
    });
    return () => {
      u.then((un) => un()).catch(() => {});
    };
  }, []);

  // Listen for ui_action events from the orchestrator: open the chat preview window
  // when an agent tool requests it. The window itself is a separate Tauri WebviewWindow
  // hash-routed at /#/chat-preview.
  useEffect(() => {
    const u = onToolEvent((ev) => {
      if (ev.kind !== "ui_action") return;
      if (ev.data.action !== "open_chat_preview") return;
      const label = "chat-preview";
      try {
        const existing = WebviewWindow.getByLabel(label);
        existing.then((w) => {
          if (w) {
            w.setFocus().catch(() => {});
            return;
          }
          new WebviewWindow(label, {
            url: "/#/chat-preview",
            title: "NarrowMind chat preview",
            width: 640,
            height: 760,
            resizable: true,
          });
        });
      } catch {
        // best-effort; Tauri will log create failures itself
      }
    });
    return () => {
      u.then((un) => un()).catch(() => {});
    };
  }, []);

  async function handleSend(text: string) {
    if (!termRef.current) return;
    termRef.current.writeUserPrompt(text);
    setBusy(true);
    try {
      await agent.sendMessage(text);
    } catch (e) {
      termRef.current.writeError(`${e}`);
    } finally {
      setBusy(false);
    }
  }

  /**
   * Open the chat preview window directly, bypassing the agent loop.
   * Everything from here on talks only to the local Qwen — no Anthropic spend.
   * Idempotent: if the window already exists we just focus it; if the
   * inference server is already up we reuse it. Bootstrap call ensures the
   * window doesn't open into an empty-context "no endpoint" state.
   */
  async function handleOpenLocalChat() {
    if (!selectedProject || localChatBusy) return;
    setLocalChatBusy(true);
    try {
      const ctx = await chat.bootstrap();
      // KARAR 1: a training run owns the VRAM. Don't kill it for chat —
      // report honestly and leave the run alone.
      if (ctx.training_active) {
        termRef.current?.writeError(
          `Training in progress — chat unavailable until it completes (run ${ctx.run_id ?? "?"}, step ${ctx.step ?? 0}/${ctx.total_steps ?? 0}). Stop it with stop_training if you need the GPU now.`,
        );
        setLocalChatBusy(false);
        return;
      }
    } catch (e) {
      termRef.current?.writeError(`local chat: ${e}`);
      setLocalChatBusy(false);
      return;
    }
    const label = "chat-preview";
    try {
      const existing = await WebviewWindow.getByLabel(label);
      if (existing) {
        await existing.setFocus();
      } else {
        new WebviewWindow(label, {
          url: "/#/chat-preview",
          title: "NarrowMind chat preview",
          width: 640,
          height: 760,
          resizable: true,
        });
      }
    } catch (e) {
      termRef.current?.writeError(`open chat window: ${e}`);
    } finally {
      setLocalChatBusy(false);
    }
  }

  const ready = Boolean(selectedProject) && hasKey === true;
  const placeholder = !hasKey
    ? "set your Anthropic API key in Settings first (⚙)"
    : !selectedProject
      ? "select or create a project in the left rail first"
      : undefined;

  return (
    <div className="app">
      <ProjectRail
        onOpenSettings={() => setSettingsOpen(true)}
        onSelectionChanged={setSelectedProject}
      />

      <main className="center">
        <header className="banner">
          <span className="tag">phase 1</span>
          {selectedProject ? (
            <span>
              project <strong>{selectedProject}</strong>
            </span>
          ) : (
            <span className="muted">no project selected</span>
          )}
          <span className="spacer" />
          {(sessionUsage.input_tokens > 0 || sessionUsage.output_tokens > 0) && (
            <span
              className="usage-meter"
              title={`Agent-loop API usage this session: ${sessionUsage.input_tokens.toLocaleString()} input / ${sessionUsage.output_tokens.toLocaleString()} output tokens. Estimate at Sonnet-class rates — Local Chat costs nothing and is not counted.`}
            >
              {formatTokens(sessionUsage.input_tokens)} in /{" "}
              {formatTokens(sessionUsage.output_tokens)} out · ~
              {formatUsd(estimateUsd(sessionUsage))}
            </span>
          )}
          {selectedProject && (
            <button
              type="button"
              className="banner-action"
              disabled={localChatBusy}
              onClick={handleOpenLocalChat}
              title="Open the local Qwen chat window. Bypasses the agent loop — no Anthropic API spend."
            >
              {localChatBusy ? "starting…" : "💬 Local chat"}
            </button>
          )}
          {hasKey === false && (
            <button
              type="button"
              className="primary"
              onClick={() => setSettingsOpen(true)}
            >
              Set API key
            </button>
          )}
        </header>

        <TerminalPane onReady={onTerminalReady} />

        <InputBar disabled={busy || !ready} placeholder={placeholder} onSubmit={handleSend} />
      </main>

      <RightSidebar />

      <SettingsDialog
        open={settingsOpen}
        onClose={() => setSettingsOpen(false)}
        selectedProject={selectedProject}
      />
    </div>
  );
}
