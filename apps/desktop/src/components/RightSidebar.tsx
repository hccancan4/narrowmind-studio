import { useState } from "react";

type Tab = "dataset" | "training" | "eval";

const TABS: { id: Tab; label: string; phase: string }[] = [
  { id: "dataset", label: "Dataset", phase: "Phase 2" },
  { id: "training", label: "Training", phase: "Phase 4" },
  { id: "eval", label: "Eval", phase: "Phase 5" },
];

export function RightSidebar() {
  const [tab, setTab] = useState<Tab>("dataset");

  return (
    <aside className="right-sidebar">
      <nav>
        {TABS.map((t) => (
          <button
            key={t.id}
            type="button"
            className={t.id === tab ? "active" : ""}
            onClick={() => setTab(t.id)}
          >
            {t.label}
          </button>
        ))}
      </nav>
      <div className="tab-body">
        <h3>{TABS.find((t) => t.id === tab)?.label}</h3>
        <p className="muted">
          Mock placeholder. Real content lands in {TABS.find((t) => t.id === tab)?.phase} per
          docs/ROADMAP.md.
        </p>
        {tab === "dataset" && (
          <ul className="mock-list">
            <li>chunks (virtualised list)</li>
            <li>per-source filter</li>
            <li>include / exclude toggles</li>
            <li>synth Q&amp;A previews</li>
          </ul>
        )}
        {tab === "training" && (
          <ul className="mock-list">
            <li>live loss / lr / grad-norm chart</li>
            <li>checkpoint list with "best" marker</li>
            <li>GPU mem watch + ETA</li>
            <li>log tail</li>
          </ul>
        )}
        {tab === "eval" && (
          <ul className="mock-list">
            <li>A/B/C/D base/RAG/LoRA/hybrid</li>
            <li>1–5 manual rating UI</li>
            <li>LLM-judge auto scores</li>
            <li>markdown report export</li>
          </ul>
        )}
      </div>
    </aside>
  );
}
