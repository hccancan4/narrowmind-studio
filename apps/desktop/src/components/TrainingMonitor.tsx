import { useCallback, useEffect, useMemo, useState } from "react";
import {
  CartesianGrid,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";

import { onTrainingEvent, type TrainingMetric } from "../lib/events";
import { training, type TrainingRun } from "../lib/tauri";

/** Phase 4 Training Monitor — realizes the Phase 1 mock.
 *
 * Data model: history loads from runs/<id>/metrics.jsonl via the
 * training_metrics command (filesystem is the source of truth — this is what
 * makes past runs visible after an app restart), then live training:event
 * metrics append on top for the active run. */
export function TrainingMonitor() {
  const [runs, setRuns] = useState<TrainingRun[]>([]);
  const [selectedRun, setSelectedRun] = useState<string | null>(null);
  const [metrics, setMetrics] = useState<TrainingMetric[]>([]);
  const [logLines, setLogLines] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [stopBusy, setStopBusy] = useState(false);

  const refreshRuns = useCallback(async () => {
    try {
      const r = await training.runs();
      setRuns(r.runs);
      // Auto-select the newest run when nothing is selected yet.
      setSelectedRun((cur) => cur ?? r.runs[0]?.run_id ?? null);
    } catch (e) {
      setError(`${e}`);
    }
  }, []);

  // Initial + periodic run-list refresh (status fields change on epoch saves).
  useEffect(() => {
    refreshRuns();
    const t = setInterval(refreshRuns, 5000);
    return () => clearInterval(t);
  }, [refreshRuns]);

  // Load metric history + log tail whenever the selected run changes.
  useEffect(() => {
    if (!selectedRun) {
      setMetrics([]);
      setLogLines([]);
      return;
    }
    let cancelled = false;
    training
      .metrics(selectedRun)
      .then((m) => {
        if (!cancelled) setMetrics(m.metrics);
      })
      .catch((e) => setError(`${e}`));
    training
      .logTail(selectedRun, 50)
      .then((l) => {
        if (!cancelled) setLogLines(l.lines);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [selectedRun]);

  // Live stream: append metrics for the selected run; refresh on finish.
  useEffect(() => {
    const u = onTrainingEvent((ev) => {
      if (ev.kind === "metric" && ev.run_id === selectedRun) {
        setMetrics((prev) => [...prev, ev.data]);
      }
      if (ev.kind === "finished") {
        refreshRuns();
      }
    });
    return () => {
      u.then((un) => un()).catch(() => {});
    };
  }, [selectedRun, refreshRuns]);

  // Periodic log tail refresh for the running run only.
  const selected = runs.find((r) => r.run_id === selectedRun);
  useEffect(() => {
    if (!selectedRun || selected?.status !== "running") return;
    const t = setInterval(() => {
      training
        .logTail(selectedRun, 50)
        .then((l) => setLogLines(l.lines))
        .catch(() => {});
    }, 3000);
    return () => clearInterval(t);
  }, [selectedRun, selected?.status]);

  const latest = metrics.at(-1);
  const chartData = useMemo(
    () =>
      metrics
        .filter((m) => m.loss != null || m.eval_loss != null)
        .map((m) => ({
          step: m.step,
          loss: m.loss,
          eval_loss: m.eval_loss ?? null,
          lr: m.lr,
          grad_norm: m.grad_norm,
        })),
    [metrics],
  );

  async function handleStop() {
    setStopBusy(true);
    try {
      await training.stop();
    } catch (e) {
      setError(`${e}`);
    } finally {
      setStopBusy(false);
    }
  }

  if (runs.length === 0) {
    return (
      <div className="tm-empty muted">
        no training runs yet — ask the agent to <code>start_training</code> once the
        SFT dataset is built
      </div>
    );
  }

  return (
    <div className="training-monitor">
      <div className="tm-toolbar">
        <select
          value={selectedRun ?? ""}
          onChange={(e) => setSelectedRun(e.target.value)}
        >
          {runs.map((r) => (
            <option key={r.run_id} value={r.run_id}>
              {r.run_id.slice(0, 8)} · {r.status}
            </option>
          ))}
        </select>
        {selected?.status === "running" && (
          <button type="button" disabled={stopBusy} onClick={handleStop}>
            {stopBusy ? "stopping…" : "stop"}
          </button>
        )}
      </div>

      {selected && (
        <div className="tm-stats">
          <span className={`tag tm-status-${selected.status}`}>{selected.status}</span>
          <span title="step">
            {latest?.step ?? selected.step ?? 0}/{latest?.total_steps ?? selected.total_steps ?? 0}
          </span>
          {latest?.eta_secs != null && selected.status === "running" && (
            <span title="ETA">~{formatEta(latest.eta_secs)}</span>
          )}
          {latest?.gpu_mem_mb != null && latest.gpu_mem_mb > 0 && (
            <span title="GPU memory">{(latest.gpu_mem_mb / 1024).toFixed(1)} GB</span>
          )}
          {selected.best_eval_loss != null && (
            <span title="best eval_loss">best {Number(selected.best_eval_loss).toFixed(4)}</span>
          )}
        </div>
      )}

      {chartData.length > 0 ? (
        <>
          <div className="tm-chart">
            <ResponsiveContainer width="100%" height={140}>
              <LineChart data={chartData} margin={{ top: 4, right: 8, bottom: 0, left: -18 }}>
                <CartesianGrid stroke="#2a2a30" strokeDasharray="3 3" />
                <XAxis dataKey="step" stroke="#8c8c93" fontSize={10} />
                <YAxis stroke="#8c8c93" fontSize={10} domain={["auto", "auto"]} />
                <Tooltip
                  contentStyle={{ background: "#14141a", border: "1px solid #2a2a30", fontSize: 11 }}
                />
                <Line type="monotone" dataKey="loss" stroke="#6cb6ff" dot={false} strokeWidth={1.5} isAnimationActive={false} />
                <Line type="monotone" dataKey="eval_loss" stroke="#86c270" dot={true} strokeWidth={1.5} isAnimationActive={false} connectNulls />
              </LineChart>
            </ResponsiveContainer>
            <div className="tm-legend muted">
              <span style={{ color: "#6cb6ff" }}>━ loss</span>{" "}
              <span style={{ color: "#86c270" }}>● eval_loss</span>
            </div>
          </div>
          <div className="tm-chart">
            <ResponsiveContainer width="100%" height={90}>
              <LineChart data={chartData} margin={{ top: 4, right: 8, bottom: 0, left: -18 }}>
                <CartesianGrid stroke="#2a2a30" strokeDasharray="3 3" />
                <XAxis dataKey="step" stroke="#8c8c93" fontSize={10} />
                <YAxis stroke="#8c8c93" fontSize={10} domain={["auto", "auto"]} />
                <Tooltip
                  contentStyle={{ background: "#14141a", border: "1px solid #2a2a30", fontSize: 11 }}
                />
                <Line type="monotone" dataKey="grad_norm" stroke="#e9c46a" dot={false} strokeWidth={1} isAnimationActive={false} />
                <Line type="monotone" dataKey="lr" stroke="#c792ea" dot={false} strokeWidth={1} isAnimationActive={false} />
              </LineChart>
            </ResponsiveContainer>
            <div className="tm-legend muted">
              <span style={{ color: "#e9c46a" }}>━ grad_norm</span>{" "}
              <span style={{ color: "#c792ea" }}>━ lr</span>
            </div>
          </div>
        </>
      ) : (
        <div className="muted tm-waiting">
          {selected?.status === "running"
            ? "waiting for the first metric (model load + compile can take a few minutes)…"
            : "no metrics recorded for this run"}
        </div>
      )}

      {logLines.length > 0 && (
        <div className="tm-log">
          {logLines.map((l, i) => (
            <div key={i}>{l}</div>
          ))}
        </div>
      )}

      {error && <div className="ds-error">{error}</div>}
    </div>
  );
}

function formatEta(secs: number): string {
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.round(secs / 60)}m`;
  return `${(secs / 3600).toFixed(1)}h`;
}
