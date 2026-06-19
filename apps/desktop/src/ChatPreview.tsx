import { useCallback, useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import {
  chat,
  type ChatHit,
  type ChatPreviewContext,
  type ServeableModel,
} from "./lib/tauri";
import "./index.css";

type Message = { role: "user" | "assistant"; text: string; hits?: ChatHit[] };

export function ChatPreview() {
  const [ctx, setCtx] = useState<ChatPreviewContext | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [models, setModels] = useState<ServeableModel[]>([]);
  const [modelKey, setModelKey] = useState<string | null>(null);
  const [switching, setSwitching] = useState(false);
  const scrollerRef = useRef<HTMLDivElement | null>(null);

  // Load context (project + endpoint) + the serveable-model list for the picker.
  useEffect(() => {
    chat
      .context()
      .then((c) => {
        setCtx(c);
        setModelKey(c.model_key ?? null);
      })
      .catch((e) => setError(`${e}`));
    chat
      .models()
      .then((r) => {
        setModels(r.models);
        setModelKey((k) => k ?? r.current);
      })
      .catch(() => {});
  }, []);

  // Switch the served model: restarts the inference server on the chosen model
  // (a model load takes a few seconds) and starts a fresh conversation.
  const switchModel = useCallback(
    async (key: string) => {
      if (!key || key === modelKey || switching || busy) return;
      setSwitching(true);
      setError(null);
      try {
        const c = await chat.bootstrap(key);
        setCtx(c);
        setModelKey(c.model_key ?? key);
        setMessages([]);
      } catch (e) {
        setError(`switch model: ${e}`);
        // Re-sync to whatever is actually serving — the server-side fix keeps
        // the previous model up on a pre-load failure, so don't leave the header
        // pointing at a model that never started.
        chat
          .context()
          .then((c) => {
            setCtx(c);
            setModelKey(c.model_key ?? null);
          })
          .catch(() => {});
      } finally {
        setSwitching(false);
      }
    },
    [modelKey, switching, busy],
  );

  // Stream wiring: register listeners once.
  useEffect(() => {
    const unsubs: Promise<UnlistenFn>[] = [];
    unsubs.push(
      listen<string>("chat-preview:token", (e) => {
        setMessages((prev) => {
          if (prev.length === 0 || prev[prev.length - 1]!.role !== "assistant") return prev;
          const last = prev[prev.length - 1]!;
          return [
            ...prev.slice(0, -1),
            { ...last, text: last.text + e.payload },
          ];
        });
      }),
    );
    unsubs.push(
      listen<ChatHit[]>("chat-preview:hits", (e) => {
        setMessages((prev) => {
          if (prev.length === 0 || prev[prev.length - 1]!.role !== "assistant") return prev;
          const last = prev[prev.length - 1]!;
          return [...prev.slice(0, -1), { ...last, hits: e.payload }];
        });
      }),
    );
    unsubs.push(
      listen<{ answer: string }>("chat-preview:done", () => {
        setBusy(false);
      }),
    );
    unsubs.push(
      listen<{ stage: string; error: string }>("chat-preview:error", (e) => {
        setError(`${e.payload.stage}: ${e.payload.error}`);
        setBusy(false);
      }),
    );
    return () => {
      unsubs.forEach((p) => p.then((u) => u()).catch(() => {}));
    };
  }, []);

  // Auto-scroll on new content.
  useEffect(() => {
    if (scrollerRef.current) scrollerRef.current.scrollTop = scrollerRef.current.scrollHeight;
  }, [messages]);

  const handleSend = useCallback(async () => {
    const query = input.trim();
    if (!query || busy) return;
    setError(null);
    setInput("");
    setMessages((prev) => [
      ...prev,
      { role: "user", text: query },
      { role: "assistant", text: "" },
    ]);
    setBusy(true);
    try {
      await chat.send(query);
    } catch (e) {
      setError(`${e}`);
      setBusy(false);
    }
  }, [input, busy]);

  return (
    <div className="chat-preview">
      <header>
        <span className="tag">chat preview</span>
        <span className="project-name">{ctx?.project ?? "(no project)"}</span>
        {models.length > 0 && (
          <select
            className="model-select"
            value={modelKey ?? ""}
            disabled={switching || busy}
            title="serving model — switch to A/B base vs your fine-tune"
            onChange={(e) => switchModel(e.target.value)}
            style={{
              background: "#1e1e1e",
              color: "#ddd",
              border: "1px solid #444",
              borderRadius: 4,
              padding: "2px 6px",
              fontSize: "0.82em",
              maxWidth: "16rem",
            }}
          >
            {modelKey === null && <option value="">(starting…)</option>}
            {modelKey !== null && !models.some((m) => m.key === modelKey) && (
              // The live model isn't in the serveable list (e.g. a domain GGUF
              // deleted out-of-band while serving). Keep the control reflecting
              // reality instead of rendering an empty selection.
              <option value={modelKey} disabled>
                {`(current: ${modelKey})`}
              </option>
            )}
            {models.map((m) => (
              <option key={m.key} value={m.key}>
                {m.label}
              </option>
            ))}
          </select>
        )}
        {switching && <span className="model" style={{ opacity: 0.7 }}>switching…</span>}
        {ctx?.endpoint && (
          <span className="endpoint" title={ctx.endpoint}>
            {ctx.endpoint}
          </span>
        )}
      </header>

      <div ref={scrollerRef} className="chat-scroller">
        {messages.length === 0 && (
          <div className="chat-empty">
            <div style={{ fontSize: "1.6rem", marginBottom: "0.5rem" }}>💬</div>
            <div>
              ask your local DSLM anything.<br />
              <span style={{ opacity: 0.7 }}>
                answers stream from Qwen with citations from this project's RAG index.
              </span>
            </div>
          </div>
        )}
        {messages.map((m, i) => (
          <div key={i} className={`chat-msg ${m.role}`}>
            <div className="chat-bubble">{m.text || (m.role === "assistant" && busy ? "…" : "")}</div>
            {m.role === "assistant" && m.hits && m.hits.length > 0 && (
              <details className="chat-cites">
                <summary>{m.hits.length} citations</summary>
                <ul>
                  {m.hits.map((h, k) => (
                    <li key={h.chunk_id}>
                      <span className="cite-num">[chunk {k + 1}]</span>{" "}
                      <span className="cite-src">{h.source_id}</span>
                      <span className="cite-text">{h.text.slice(0, 200)}…</span>
                    </li>
                  ))}
                </ul>
              </details>
            )}
          </div>
        ))}
      </div>

      {error && <div className="chat-error">{error}</div>}

      <form
        className="chat-input"
        onSubmit={(e) => {
          e.preventDefault();
          handleSend();
        }}
      >
        <textarea
          rows={2}
          value={input}
          disabled={busy}
          placeholder={busy ? "streaming…" : "ask your DSLM (enter to send, shift+enter newline)"}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              handleSend();
            }
          }}
        />
        <button type="submit" disabled={busy || !input.trim()}>
          send
        </button>
      </form>
    </div>
  );
}
