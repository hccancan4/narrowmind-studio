import { useEffect, useState } from "react";

import { settings } from "../lib/tauri";

type Props = {
  open: boolean;
  onClose: () => void;
};

const PROVIDERS = [{ id: "anthropic", label: "Anthropic (Claude)" }] as const;

export function SettingsDialog({ open, onClose }: Props) {
  const [provider, setProvider] = useState<typeof PROVIDERS[number]["id"]>("anthropic");
  const [apiKey, setApiKey] = useState("");
  const [hasKey, setHasKey] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setApiKey("");
    setError(null);
    setBusy(true);
    settings
      .hasProviderKey(provider)
      .then((b) => setHasKey(b))
      .catch((e) => setError(`${e}`))
      .finally(() => setBusy(false));
  }, [open, provider]);

  if (!open) return null;

  async function handleSave() {
    if (!apiKey.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await settings.setProviderKey(provider, apiKey.trim());
      setHasKey(true);
      setApiKey("");
    } catch (e) {
      setError(`${e}`);
    } finally {
      setBusy(false);
    }
  }

  async function handleDelete() {
    const ok = window.confirm(`Remove the stored ${provider} API key from the OS keychain?`);
    if (!ok) return;
    setBusy(true);
    setError(null);
    try {
      await settings.deleteProviderKey(provider);
      setHasKey(false);
    } catch (e) {
      setError(`${e}`);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <header>
          <h2>Settings</h2>
          <button type="button" className="icon-btn" onClick={onClose}>
            ✕
          </button>
        </header>
        <section>
          <label>
            <span>Provider</span>
            <select
              value={provider}
              onChange={(e) => setProvider(e.target.value as typeof provider)}
              disabled={busy}
            >
              {PROVIDERS.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.label}
                </option>
              ))}
            </select>
          </label>
          <p className="muted">v1 ships Anthropic only. OpenAI / Ollama land in Phase 7.</p>
        </section>
        <section>
          <label>
            <span>
              API key{" "}
              {hasKey === true && <span className="tag green">stored</span>}
              {hasKey === false && <span className="tag muted">not set</span>}
            </span>
            <input
              type="password"
              placeholder={hasKey ? "(stored in OS keychain — paste to replace)" : "sk-ant-..."}
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              disabled={busy}
              autoComplete="off"
            />
          </label>
          <p className="muted">
            Keys are stored in your OS keychain (Windows Credential Manager / macOS
            Keychain / libsecret). They never touch the project files or logs.
          </p>
        </section>
        {error && <div className="modal-error">{error}</div>}
        <footer>
          <button type="button" onClick={handleDelete} disabled={busy || hasKey !== true}>
            Remove key
          </button>
          <span style={{ flex: 1 }} />
          <button type="button" onClick={onClose} disabled={busy}>
            Close
          </button>
          <button
            type="button"
            className="primary"
            disabled={busy || !apiKey.trim()}
            onClick={handleSave}
          >
            Save key
          </button>
        </footer>
      </div>
    </div>
  );
}
