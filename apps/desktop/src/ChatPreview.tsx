import { useCallback, useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import { chat, type ChatHit, type ChatPreviewContext } from "./lib/tauri";
import "./index.css";

type Message = { role: "user" | "assistant"; text: string; hits?: ChatHit[] };

export function ChatPreview() {
  const [ctx, setCtx] = useState<ChatPreviewContext | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const scrollerRef = useRef<HTMLDivElement | null>(null);

  // Load context (project + endpoint).
  useEffect(() => {
    chat.context().then(setCtx).catch((e) => setError(`${e}`));
  }, []);

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
        <div className="tag">chat preview</div>
        <span>
          project <strong>{ctx?.project ?? "(none)"}</strong>
          {ctx?.endpoint && <span className="muted"> · {ctx.endpoint}</span>}
        </span>
        {ctx?.filename && <span className="muted small">{ctx.filename}</span>}
      </header>

      <div ref={scrollerRef} className="chat-scroller">
        {messages.length === 0 && (
          <div className="chat-empty">
            ask anything — answers stream from your local DSLM with citations from the
            project's RAG index.
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
