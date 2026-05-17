import { useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";

import { chunks, type ChunkRecord, type ListChunksResponse } from "../lib/tauri";

type ShowFilter = "all" | "included" | "excluded";

// Initial guess only — actual row height is measured per row via
// `virtualizer.measureElement` because the text preview wraps to a variable
// number of lines depending on panel width. Using a fixed estimate caused
// rows to overlap whenever the wrapped text exceeded the guess.
const ESTIMATED_ROW_HEIGHT = 140;
const PAGE_LIMIT = 500;

export function DatasetBrowser() {
  const [data, setData] = useState<ListChunksResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [search, setSearch] = useState("");
  const [show, setShow] = useState<ShowFilter>("all");
  const parentRef = useRef<HTMLDivElement | null>(null);

  // Refresh on filter change or every 3s (so newly-ingested chunks appear).
  useEffect(() => {
    let cancelled = false;

    async function refresh() {
      setBusy(true);
      setError(null);
      try {
        const res = await chunks.list({ search: search || undefined, show, limit: PAGE_LIMIT });
        if (!cancelled) setData(res);
      } catch (e) {
        if (!cancelled) setError(`${e}`);
      } finally {
        if (!cancelled) setBusy(false);
      }
    }

    refresh();
    const t = setInterval(refresh, 3000);
    return () => {
      cancelled = true;
      clearInterval(t);
    };
  }, [search, show]);

  const items = data?.chunks ?? [];

  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => ESTIMATED_ROW_HEIGHT,
    overscan: 8,
    // Measure every row's real height so wrapped text doesn't overlap the next
    // row. Tanstack Virtual will call this with the row DOM node when it
    // mounts (via the ref callback below) and use the bbox height instead of
    // the estimate.
    measureElement:
      typeof ResizeObserver !== "undefined"
        ? (element) => element.getBoundingClientRect().height
        : undefined,
  });

  async function toggleInclude(c: ChunkRecord) {
    setBusy(true);
    setError(null);
    try {
      await chunks.filter(c.source_id, [c.chunk_id], !c.include);
      // Optimistic local update; the next 3s poll will reconcile if it diverges.
      setData((prev) =>
        prev
          ? {
              ...prev,
              chunks: prev.chunks.map((x) =>
                x.chunk_id === c.chunk_id ? { ...x, include: !c.include } : x,
              ),
            }
          : prev,
      );
    } catch (e) {
      setError(`${e}`);
    } finally {
      setBusy(false);
    }
  }

  const summary = useMemo(() => {
    if (!data) return "loading…";
    return `${data.matched} matched · ${data.scanned} scanned · ${data.returned} shown`;
  }, [data]);

  return (
    <div className="dataset-browser">
      <div className="ds-toolbar">
        <input
          type="text"
          placeholder="search chunks…"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
        />
        <select value={show} onChange={(e) => setShow(e.target.value as ShowFilter)}>
          <option value="all">all</option>
          <option value="included">included</option>
          <option value="excluded">excluded</option>
        </select>
      </div>
      <div className="ds-summary">
        {busy ? "↻ " : ""}
        {summary}
      </div>
      {error && <div className="ds-error">{error}</div>}
      <div ref={parentRef} className="ds-virtual-host">
        <div style={{ height: `${virtualizer.getTotalSize()}px`, position: "relative" }}>
          {virtualizer.getVirtualItems().map((vrow) => {
            const c = items[vrow.index];
            if (!c) return null;
            return (
              <div
                key={c.chunk_id}
                // Both the ref AND data-index are required so the virtualizer
                // can identify which row's height we're reporting.
                ref={virtualizer.measureElement}
                data-index={vrow.index}
                className={`ds-row ${c.include ? "included" : "excluded"}`}
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  width: "100%",
                  transform: `translateY(${vrow.start}px)`,
                }}
              >
                <div className="ds-row-header">
                  <span className="ds-chunk-id">{c.chunk_id}</span>
                  <span className="ds-tokens">{c.token_count}t</span>
                  <span className="ds-source">{c.source_id}</span>
                  <button
                    type="button"
                    className="ds-include-btn"
                    title={c.include ? "exclude this chunk" : "include this chunk"}
                    onClick={() => toggleInclude(c)}
                  >
                    {c.include ? "✓" : "✕"}
                  </button>
                </div>
                <div className="ds-row-text">{c.text.slice(0, 240)}{c.text.length > 240 ? "…" : ""}</div>
              </div>
            );
          })}
        </div>
        {items.length === 0 && !busy && (
          <div className="ds-empty">
            no chunks yet — ingest a source first via the agent (e.g. ask it to ingest a
            Wikipedia category or a local PDF)
          </div>
        )}
      </div>
    </div>
  );
}
