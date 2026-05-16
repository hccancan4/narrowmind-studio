import { useState } from "react";

import { DatasetBrowser } from "./DatasetBrowser";

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
        {tab === "dataset" && <DatasetBrowser />}
        {tab === "training" && (
          <>
            <h3>Training</h3>
            <p className="muted">Live training metrics arrive in Phase 4 per docs/ROADMAP.md.</p>
            <ul className="mock-list">
              <li>live loss / lr / grad-norm chart</li>
              <li>checkpoint list with "best" marker</li>
              <li>GPU mem watch + ETA</li>
              <li>log tail</li>
            </ul>
          </>
        )}
        {tab === "eval" && (
          <>
            <h3>Eval</h3>
            <p className="muted">A/B/C/D comparison + rating UI arrives in Phase 5.</p>
            <ul className="mock-list">
              <li>A/B/C/D base/RAG/LoRA/hybrid</li>
              <li>1–5 manual rating UI</li>
              <li>LLM-judge auto scores</li>
              <li>markdown report export</li>
            </ul>
          </>
        )}
      </div>
    </aside>
  );
}
