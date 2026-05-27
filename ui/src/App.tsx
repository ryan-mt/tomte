import { useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  AgentEvent,
  Config,
  StatusResp,
  getConfig,
  getStatus,
  loginApiKey,
  loginBrowser,
  logout,
  openChatSocket,
  setConfig,
} from "./api";

type Msg =
  | { id: string; kind: "user"; text: string }
  | { id: string; kind: "assistant"; text: string; reasoning?: string }
  | {
      id: string;
      kind: "tool";
      name: string;
      args: string;
      output?: string;
      error?: boolean;
      open?: boolean;
    };

export default function App() {
  const [status, setStatus] = useState<StatusResp | null>(null);
  const [cfg, setCfg] = useState<Config | null>(null);
  const [messages, setMessages] = useState<Msg[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const wsRef = useRef<WebSocket | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const [sessionId] = useState(() => Math.random().toString(36).slice(2));

  const [loadError, setLoadError] = useState<string | null>(null);
  useEffect(() => {
    // Surface mount failures so the UI doesn't sit forever on "loading…"
    // when the backend is unreachable.
    getStatus()
      .then((s) => {
        setStatus(s);
        setLoadError(null);
      })
      .catch((e: unknown) => {
        setLoadError(e instanceof Error ? e.message : String(e));
      });
    getConfig()
      .then(setCfg)
      .catch((e: unknown) => {
        setLoadError(e instanceof Error ? e.message : String(e));
      });
  }, []);

  useEffect(() => {
    scrollRef.current?.scrollIntoView({ behavior: "smooth", block: "end" });
  }, [messages]);

  function newId() {
    return Math.random().toString(36).slice(2);
  }

  function send() {
    const text = input.trim();
    if (!text || busy) return;
    setInput("");
    setMessages((m) => [...m, { id: newId(), kind: "user", text }]);
    setBusy(true);

    // Close any prior socket before opening a new one. Without this, a
    // user double-tapping Send could end up with two live sockets whose
    // onmessage handlers race to update `messages`, and the first
    // socket's onclose later flips busy back to false while the second
    // is still streaming.
    const prev = wsRef.current;
    if (prev && prev.readyState !== WebSocket.CLOSED) {
      prev.onclose = null;
      prev.onerror = null;
      prev.onmessage = null;
      try { prev.close(); } catch {}
    }

    const ws = openChatSocket();
    wsRef.current = ws;
    ws.onopen = () => {
      ws.send(
        JSON.stringify({
          type: "start",
          session_id: sessionId,
          prompt: text,
          model: cfg?.model,
          reasoning: cfg?.reasoning_effort,
        })
      );
    };
    const asstId = newId();
    setMessages((m) => [...m, { id: asstId, kind: "assistant", text: "" }]);
    const toolMap: Record<string, string> = {};
    ws.onmessage = (e) => {
      // A malformed frame previously threw inside onmessage with no catch,
      // permanently stalling busy=true until the page was reloaded.
      let ev: AgentEvent;
      try {
        ev = JSON.parse(e.data) as AgentEvent;
      } catch {
        return;
      }
      setMessages((m) => {
        const next = [...m];
        const lastAsstIdx = next.findIndex((x) => x.id === asstId);
        switch (ev.kind) {
          case "AssistantTextDelta": {
            const last = next[lastAsstIdx];
            if (last?.kind === "assistant") {
              next[lastAsstIdx] = { ...last, text: last.text + ev.text };
            }
            return next;
          }
          case "AssistantTextDone": {
            const last = next[lastAsstIdx];
            if (last?.kind === "assistant") {
              next[lastAsstIdx] = { ...last, text: ev.text };
            }
            return next;
          }
          case "ReasoningDelta": {
            const last = next[lastAsstIdx];
            if (last?.kind === "assistant") {
              next[lastAsstIdx] = {
                ...last,
                reasoning: (last.reasoning ?? "") + ev.text,
              };
            }
            return next;
          }
          case "ToolCallStarted": {
            const id = newId();
            toolMap[ev.call_id] = id;
            next.push({
              id,
              kind: "tool",
              name: ev.name,
              args: "",
              open: true,
            });
            return next;
          }
          case "ToolCallArgsDelta": {
            const tid = toolMap[ev.call_id];
            const idx = next.findIndex((x) => x.id === tid);
            if (idx >= 0 && next[idx].kind === "tool") {
              const t = next[idx] as Extract<Msg, { kind: "tool" }>;
              next[idx] = { ...t, args: t.args + ev.delta };
            }
            return next;
          }
          case "ToolCallArgsDone": {
            const tid = toolMap[ev.call_id];
            const idx = next.findIndex((x) => x.id === tid);
            if (idx >= 0 && next[idx].kind === "tool") {
              next[idx] = { ...(next[idx] as any), args: ev.arguments };
            }
            return next;
          }
          case "ToolResult": {
            const tid = toolMap[ev.call_id];
            const idx = next.findIndex((x) => x.id === tid);
            if (idx >= 0 && next[idx].kind === "tool") {
              next[idx] = {
                ...(next[idx] as any),
                output: ev.output,
                error: ev.error,
              };
            }
            // Restart assistant bubble for next chunk after tool
            const lastIsAsst = next[next.length - 1]?.kind === "assistant";
            if (!lastIsAsst) {
              next.push({ id: newId(), kind: "assistant", text: "" });
            }
            return next;
          }
          case "TurnComplete":
            return next;
          case "Error":
            next.push({ id: newId(), kind: "assistant", text: `❌ ${ev.message}` });
            return next;
          default:
            return next;
        }
      });
    };
    ws.onclose = () => {
      setBusy(false);
    };
    ws.onerror = () => {
      setBusy(false);
    };
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  }

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="brand">
          <span className="dot" />
          opencli
        </div>

        <div className="section-title">Account</div>
        <StatusBlock
          status={status}
          onChange={async () => setStatus(await getStatus())}
        />

        <div className="section-title">Model</div>
        <ConfigBlock
          cfg={cfg}
          onSave={async (c) => {
            const next = await setConfig(c);
            setCfg(next);
          }}
        />

        <div className="section-title">Session</div>
        <button
          className="btn secondary"
          onClick={() => setMessages([])}
          disabled={busy}
        >
          Clear conversation
        </button>
      </aside>

      <main className="main">
        <div className="topbar">
          <div>
            <strong>Chat</strong>{" "}
            <span className="meta">
              · model {cfg?.model ?? "…"} · reasoning {cfg?.reasoning_effort ?? "…"}
            </span>
          </div>
          <div className="meta">
            {loadError ? `error: ${loadError}` : busy ? "working…" : "ready"}
          </div>
        </div>

        <div className="messages">
          {messages.length === 0 ? (
            <Empty
              onPick={(p) => {
                setInput(p);
              }}
            />
          ) : (
            messages.map((m) => <MsgBubble key={m.id} msg={m} />)
          )}
          <div ref={scrollRef} className="scroll-bottom" />
        </div>

        <div className="composer">
          <div className="composer-inner">
            <textarea
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={onKeyDown}
              placeholder="Ask opencli anything — write code, read files, run commands…"
              rows={1}
            />
            <button className="btn" onClick={send} disabled={busy || !input.trim()}>
              {busy ? "…" : "Send"}
            </button>
          </div>
          <div className="hint">Enter to send · Shift+Enter for newline</div>
        </div>
      </main>
    </div>
  );
}

function StatusBlock({
  status,
  onChange,
}: {
  status: StatusResp | null;
  onChange: () => Promise<void>;
}) {
  const [apiKey, setApiKey] = useState("");
  if (!status) return <div className="status">loading…</div>;
  if (status.mode === "none") {
    return (
      <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
        <div className="status">Not signed in</div>
        <button
          className="btn"
          onClick={async () => {
            await loginBrowser();
            alert("OAuth opened in your terminal. Refresh this page once finished.");
          }}
        >
          Sign in with ChatGPT
        </button>
        <div className="field">
          <label>or paste an API key</label>
          <input
            type="password"
            value={apiKey}
            onChange={(e) => setApiKey(e.target.value)}
            placeholder="sk-…"
          />
        </div>
        <button
          className="btn secondary"
          onClick={async () => {
            if (apiKey) {
              await loginApiKey(apiKey);
              setApiKey("");
              await onChange();
            }
          }}
        >
          Save key
        </button>
      </div>
    );
  }
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      <div className="status ok">
        {status.mode === "chatgpt" ? "ChatGPT" : "API key"}
        {status.account_id ? ` · ${status.account_id.slice(0, 8)}` : ""}
      </div>
      <button
        className="btn secondary"
        onClick={async () => {
          await logout();
          await onChange();
        }}
      >
        Sign out
      </button>
    </div>
  );
}

const MODELS = ["gpt-5.5", "gpt-5.5-pro", "gpt-5.4", "gpt-5.4-mini", "gpt-5.4-nano"];
const REASONING = ["low", "medium", "high", "xhigh"];
const VERBOSITY = ["low", "medium", "high"];

function ConfigBlock({
  cfg,
  onSave,
}: {
  cfg: Config | null;
  onSave: (c: Config) => Promise<void>;
}) {
  const [draft, setDraft] = useState<Config | null>(cfg);
  useEffect(() => {
    setDraft(cfg);
  }, [cfg]);
  if (!draft) return <div className="status">loading…</div>;
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      <div className="field">
        <label>Model</label>
        <select
          value={draft.model}
          onChange={(e) => setDraft({ ...draft, model: e.target.value })}
        >
          {MODELS.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
        </select>
      </div>
      <div className="field">
        <label>Reasoning effort</label>
        <select
          value={draft.reasoning_effort}
          onChange={(e) => setDraft({ ...draft, reasoning_effort: e.target.value })}
        >
          {REASONING.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
        </select>
      </div>
      <div className="field">
        <label>Verbosity</label>
        <select
          value={draft.verbosity}
          onChange={(e) => setDraft({ ...draft, verbosity: e.target.value })}
        >
          {VERBOSITY.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
        </select>
      </div>
      <button className="btn" onClick={() => onSave(draft)}>
        Save
      </button>
    </div>
  );
}

function MsgBubble({ msg }: { msg: Msg }) {
  if (msg.kind === "user") {
    return (
      <div className="msg user">
        <div className="role">
          <span className="name">You</span>
        </div>
        <ReactMarkdown remarkPlugins={[remarkGfm]}>{msg.text}</ReactMarkdown>
      </div>
    );
  }
  if (msg.kind === "assistant") {
    return (
      <div className="msg">
        {msg.reasoning ? <div className="reasoning">{msg.reasoning}</div> : null}
        <div className="role">
          <span className="name">opencli</span>
        </div>
        {msg.text ? (
          <ReactMarkdown remarkPlugins={[remarkGfm]}>{msg.text}</ReactMarkdown>
        ) : (
          <span style={{ color: "var(--muted)" }}>…</span>
        )}
      </div>
    );
  }
  return (
    <div className={`tool ${msg.error ? "err" : ""}`}>
      <div className="head">
        <span className="pill">tool</span>
        <span className="name">{msg.name}</span>
        <span className="args">
          {msg.args ? compactArgs(msg.args) : "…"}
        </span>
      </div>
      {msg.output ? (
        <pre>{msg.output}</pre>
      ) : null}
    </div>
  );
}

function compactArgs(s: string) {
  try {
    const v = JSON.parse(s);
    return Object.entries(v)
      .map(([k, val]) => `${k}=${JSON.stringify(val).slice(0, 50)}`)
      .join(" ");
  } catch {
    return s.slice(0, 100);
  }
}

function Empty({ onPick }: { onPick: (s: string) => void }) {
  const suggestions = [
    "List the files in the current directory",
    "Read CLAUDE.md and summarize it",
    "Find every TODO comment in the source tree",
    "Create hello.txt containing the word 'Hello'",
  ];
  return (
    <div className="empty">
      <h2>Ready to help you code</h2>
      <p>Type a question below, or try one of the suggestions.</p>
      <div className="suggest">
        {suggestions.map((s) => (
          <button key={s} onClick={() => onPick(s)}>
            {s}
          </button>
        ))}
      </div>
    </div>
  );
}
