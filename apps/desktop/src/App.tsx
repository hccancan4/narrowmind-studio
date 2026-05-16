import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type HelloResponse = {
  message: string;
  worker_version: string;
  worker_pid: number;
  python_version: string;
  platform: string;
};

type LogLine = { kind: "out" | "in" | "err"; text: string };

export function App() {
  const [log, setLog] = useState<LogLine[]>([]);
  const [busy, setBusy] = useState(false);

  function append(...lines: LogLine[]) {
    setLog((prev) => [...prev, ...lines]);
  }

  async function runHello() {
    setBusy(true);
    append({ kind: "out", text: "> hello round-trip starting (Tauri → Rust orchestrator → Python worker)" });
    try {
      const result = await invoke<HelloResponse>("hello_round_trip_cmd", { name: "tauri" });
      append(
        { kind: "in", text: `< ${result.message}` },
        { kind: "in", text: `  worker_version=${result.worker_version}` },
        { kind: "in", text: `  worker_pid=${result.worker_pid}` },
        { kind: "in", text: `  python_version=${result.python_version}` },
        { kind: "in", text: `  platform=${result.platform}` },
      );
    } catch (e) {
      append({ kind: "err", text: `ERROR: ${e instanceof Error ? e.message : String(e)}` });
    } finally {
      setBusy(false);
    }
  }

  return (
    <main>
      <header>
        <h1>
          <span className="tag">phase 0</span>
          NarrowMind Studio
        </h1>
        <p>
          Bootstrap shell. The debug button below spawns a Python worker via the Rust orchestrator
          and round-trips a JSON-RPC <code>hello</code> call.
        </p>
      </header>

      <div>
        <button type="button" onClick={runHello} disabled={busy}>
          {busy ? "Running…" : "Run hello round-trip"}
        </button>
      </div>

      <pre>
        {log.length === 0
          ? "(no calls yet — click the button to round-trip a hello)"
          : log.map((l) => l.text).join("\n")}
      </pre>
    </main>
  );
}
