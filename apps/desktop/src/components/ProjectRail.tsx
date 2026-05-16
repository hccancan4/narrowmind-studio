import { useEffect, useState } from "react";

import { projects, type ProjectSummary } from "../lib/tauri";

type Props = {
  onOpenSettings: () => void;
  onSelectionChanged: (name: string | null) => void;
};

export function ProjectRail({ onOpenSettings, onSelectionChanged }: Props) {
  const [list, setList] = useState<ProjectSummary[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [newName, setNewName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function refresh() {
    try {
      const [list, current] = await Promise.all([projects.list(), projects.current()]);
      setList(list);
      setSelected(current);
      onSelectionChanged(current);
    } catch (e) {
      setError(`${e}`);
    }
  }

  useEffect(() => {
    refresh();
    // Refresh once a second so projects the agent creates show up automatically.
    const i = setInterval(refresh, 1000);
    return () => clearInterval(i);
  }, []);

  async function handleCreate() {
    const name = newName.trim();
    if (!name) return;
    setBusy(true);
    setError(null);
    try {
      await projects.create(name, "rag");
      setNewName("");
      await refresh();
    } catch (e) {
      setError(`${e}`);
    } finally {
      setBusy(false);
    }
  }

  async function handleSelect(name: string) {
    setBusy(true);
    setError(null);
    try {
      await projects.select(name);
      setSelected(name);
      onSelectionChanged(name);
    } catch (e) {
      setError(`${e}`);
    } finally {
      setBusy(false);
    }
  }

  async function handleDelete(name: string) {
    const ok = window.confirm(`Delete project '${name}' and all its data? This cannot be undone.`);
    if (!ok) return;
    setBusy(true);
    setError(null);
    try {
      await projects.remove(name);
      await refresh();
    } catch (e) {
      setError(`${e}`);
    } finally {
      setBusy(false);
    }
  }

  return (
    <aside className="left-rail">
      <header>
        <h2>Projects</h2>
        <button
          type="button"
          className="icon-btn"
          title="Settings"
          onClick={onOpenSettings}
        >
          ⚙
        </button>
      </header>

      <div className="rail-section">
        <div className="rail-row">
          <input
            type="text"
            placeholder="new-project-name"
            value={newName}
            disabled={busy}
            onChange={(e) => setNewName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") handleCreate();
            }}
          />
          <button type="button" disabled={busy || !newName.trim()} onClick={handleCreate}>
            +
          </button>
        </div>
      </div>

      <ul className="project-list">
        {list.length === 0 ? (
          <li className="muted">(no projects yet)</li>
        ) : (
          list.map((p) => (
            <li
              key={p.name}
              className={p.name === selected ? "selected" : ""}
              onClick={() => handleSelect(p.name)}
            >
              <div className="project-row">
                <span className="project-name">{p.name}</span>
                <button
                  type="button"
                  className="icon-btn destructive"
                  title="Delete project"
                  onClick={(e) => {
                    e.stopPropagation();
                    handleDelete(p.name);
                  }}
                >
                  ✕
                </button>
              </div>
              <div className="project-meta">
                <span className="tag">{p.tier}</span>
                <span className="tag muted">{p.status}</span>
              </div>
            </li>
          ))
        )}
      </ul>

      {error && <div className="rail-error">{error}</div>}
    </aside>
  );
}
