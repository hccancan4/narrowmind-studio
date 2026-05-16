import { useCallback, useEffect, useRef, useState } from "react";

import { InputBar } from "./components/InputBar";
import { ProjectRail } from "./components/ProjectRail";
import { RightSidebar } from "./components/RightSidebar";
import { SettingsDialog } from "./components/SettingsDialog";
import { TerminalPane, type TerminalHandle } from "./components/TerminalPane";
import { agent, settings } from "./lib/tauri";

export function App() {
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [selectedProject, setSelectedProject] = useState<string | null>(null);
  const [hasKey, setHasKey] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const termRef = useRef<TerminalHandle | null>(null);

  const onTerminalReady = useCallback((handle: TerminalHandle) => {
    termRef.current = handle;
  }, []);

  useEffect(() => {
    settings.hasProviderKey("anthropic").then(setHasKey).catch(() => setHasKey(false));
  }, [settingsOpen]);

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

      <SettingsDialog open={settingsOpen} onClose={() => setSettingsOpen(false)} />
    </div>
  );
}
