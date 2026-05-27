export type StatusResp = {
  mode: "none" | "api_key" | "chatgpt";
  account_id: string | null;
  model: string;
};

export type Config = {
  model: string;
  reasoning_effort: string;
  verbosity: string;
  auto_approve_read: boolean;
  auto_approve_write: boolean;
};

export type TodoStatus = "pending" | "in_progress" | "completed";

export type TodoItem = {
  content: string;
  status: TodoStatus;
  active_form: string;
};

export type AgentEvent =
  | { kind: "AssistantTextDelta"; text: string }
  | { kind: "AssistantTextDone"; text: string }
  | { kind: "ReasoningDelta"; text: string }
  | { kind: "ReasoningDone"; text: string }
  | { kind: "ToolCallStarted"; name: string; call_id: string }
  | { kind: "ToolCallArgsDelta"; call_id: string; delta: string }
  | { kind: "ToolCallArgsDone"; call_id: string; arguments: string }
  | { kind: "ToolResult"; call_id: string; output: string; error: boolean }
  | { kind: "TodosSnapshot"; todos: TodoItem[] }
  | { kind: "Usage"; input_tokens: number; output_tokens: number; total_tokens: number }
  | { kind: "TurnComplete" }
  | { kind: "Error"; message: string }
  | { kind: "ContextWarning"; used: number; limit: number }
  | {
      kind: "ApprovalRequest";
      call_id: string;
      tool_name: string;
      args_json: string;
      diff_preview: string | null;
    }
  | { kind: "ApprovalGranted"; call_id: string }
  | { kind: "ApprovalDenied"; call_id: string };

const API = "/api";

async function jsonOk<T>(r: Response): Promise<T> {
  if (!r.ok) {
    const body = await r.text().catch(() => "");
    throw new Error(`${r.status} ${r.statusText}: ${body.slice(0, 200)}`);
  }
  return (await r.json()) as T;
}

export async function getStatus(): Promise<StatusResp> {
  const r = await fetch(`${API}/status`);
  return jsonOk<StatusResp>(r);
}

export async function getConfig(): Promise<Config> {
  const r = await fetch(`${API}/config`);
  return jsonOk<Config>(r);
}

export async function setConfig(cfg: Config): Promise<Config> {
  const r = await fetch(`${API}/config`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(cfg),
  });
  return jsonOk<Config>(r);
}

export async function loginBrowser(): Promise<void> {
  await fetch(`${API}/login`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({}),
  });
}

export async function loginApiKey(key: string): Promise<void> {
  await fetch(`${API}/login`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ api_key: key }),
  });
}

export async function logout(): Promise<void> {
  await fetch(`${API}/logout`, { method: "POST" });
}

export function openChatSocket(): WebSocket {
  const proto = window.location.protocol === "https:" ? "wss" : "ws";
  return new WebSocket(`${proto}://${window.location.host}${API}/chat`);
}
