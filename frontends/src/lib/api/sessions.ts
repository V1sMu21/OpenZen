export interface SessionInfo {
  id: string;
  name: string;
  created_at: string;
  status: string;
  message_count: number;
  project_id?: string | null;
  project_name?: string | null;
}

import { isTauri, tauriInvoke } from "./tauri";
import { fetchJson } from "./chat";

const BASE = "/api/sessions";

// Startup currently triggers sessions.load() and projects.loadAll()
// simultaneously; projects.loadAll previously made one listSessions call
// per project as well. Coalesce concurrent identical list requests so the
// startup path issues ONE all-sessions fetch + ONE projects fetch.
const inflightListSessions = new Map<string, Promise<SessionInfo[]>>();

async function fetchSessionList(projectId?: string): Promise<SessionInfo[]> {
  if (isTauri()) {
    return (await tauriInvoke("list_sessions", { projectId: projectId || null })) ?? [];
  }
  const url = projectId ? `${BASE}?project_id=${encodeURIComponent(projectId)}` : BASE;
  const res = await fetchJson(url);
  if (!res.ok) throw new Error(`Failed to list sessions: ${res.status}`);
  const data = await res.json();
  return data.sessions ?? data ?? [];
}

export function listSessions(projectId?: string): Promise<SessionInfo[]> {
  const key = projectId ?? "*";
  const existing = inflightListSessions.get(key);
  if (existing) return existing;
  const request = fetchSessionList(projectId).finally(() => {
    inflightListSessions.delete(key);
  });
  inflightListSessions.set(key, request);
  return request;
}

export async function createSession(
  name?: string,
): Promise<{ session_id: string; name: string }> {
  if (isTauri()) {
    return await tauriInvoke("create_session", { name: name || null });
  }
  const res = await fetchJson(BASE, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name: name || undefined }),
  });
  if (!res.ok) throw new Error(`Failed to create session: ${res.status}`);
  return res.json();
}

export interface SessionPageData {
  id?: string;
  name?: string;
  created_at?: string;
  status?: string;
  messages?: unknown[];
  todos?: Array<{ id: string; content: string; status: string; priority: string; order: number }>;
  total_messages?: number;
  offset?: number;
  limit?: number;
  has_more?: boolean;
  [key: string]: unknown;
}

export interface SessionPageOptions {
  /** Number of raw messages to skip from the END of the session. */
  offset?: number;
  /** Maximum raw messages to return. */
  limit?: number;
}

export async function getSession(
  id: string,
  options?: SessionPageOptions,
): Promise<SessionPageData> {
  const offset = options?.offset ?? 0;
  const limit = options?.limit;
  if (isTauri()) {
    return (await tauriInvoke("get_session", {
      id,
      offset: limit == null ? null : offset,
      limit: limit ?? null,
    })) as SessionPageData;
  }
  const params = new URLSearchParams();
  if (limit != null) {
    params.set("offset", String(offset));
    params.set("limit", String(limit));
  }
  const qs = params.toString();
  const res = await fetchJson(`${BASE}/${id}${qs ? `?${qs}` : ""}`);
  if (!res.ok) throw new Error(`Failed to get session: ${res.status}`);
  return res.json();
}
export async function deleteSession(id: string): Promise<void> {
  if (isTauri()) {
    await tauriInvoke("delete_session", { id });
    return;
  }
  const res = await fetchJson(`${BASE}/${id}`, { method: "DELETE" });
  if (!res.ok) throw new Error(`Failed to delete session: ${res.status}`);
}

export async function renameSession(id: string, name: string): Promise<void> {
  if (isTauri()) {
    await tauriInvoke("rename_session", { id, name });
    return;
  }
  const res = await fetchJson(`${BASE}/${id}`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name }),
  });
  if (!res.ok) throw new Error(`Failed to rename session: ${res.status}`);
}

export async function stopSession(id: string): Promise<void> {
  if (isTauri()) {
    await tauriInvoke("stop_session", { id });
    return;
  }
  const res = await fetchJson(`${BASE}/${id}/stop`, { method: "POST" });
  if (!res.ok) throw new Error(`Failed to stop session: ${res.status}`);
}

/** Inject a user message into a RUNNING session without interrupting it.
 *  Tauri uses the inject_message command; HTTP webui mode posts to the
 *  intervene endpoint. The agent loop picks the message up before its
 *  next LLM turn. */
export async function injectMessage(id: string, text: string): Promise<void> {
  if (isTauri()) {
    await tauriInvoke("inject_message", { sessionId: id, text });
    return;
  }
  const res = await fetchJson(`${BASE}/${id}/intervene`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ kind: "inject_info", content: text }),
  });
  if (!res.ok) throw new Error(`Failed to inject message: ${res.status}`);
}

export async function resumeSession(id: string, modelName?: string): Promise<void> {
  if (isTauri()) {
    await tauriInvoke("resume_session", { sessionId: id, modelName });
    return;
  }
  const res = await fetchJson(`${BASE}/${id}/resume`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ message: null }),
  });
  if (!res.ok) throw new Error(`Failed to resume session: ${res.status}`);
}

export async function compressSession(id: string): Promise<{
  session_id: string;
  before_chars: number;
  after_chars: number;
  saved_chars: number;
  before_tokens: number;
  after_tokens: number;
  saved_tokens: number;
  saved_pct: number;
  messages_removed: number;
  strategy: string;
}> {
  if (isTauri()) {
    return await tauriInvoke("compress_session", { id });
  }
  const res = await fetchJson(`${BASE}/${id}/compress`, { method: "POST" });
  if (!res.ok) throw new Error(`Failed to compress session: ${res.status}`);
  return res.json();
}

// ── Session-Project API ──

export async function createSessionInProject(
  projectId: string,
  name?: string,
): Promise<{ session_id: string; name: string; project_id: string }> {
  if (isTauri()) {
    return await tauriInvoke("create_session_in_project", { projectId, name: name || null });
  }
  const res = await fetchJson(`${BASE}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name, project_id: projectId }),
  });
  if (!res.ok) throw new Error(`Failed to create session in project: ${res.status}`);
  return res.json();
}

export async function moveSession(
  sessionId: string,
  projectId: string,
): Promise<void> {
  if (isTauri()) {
    await tauriInvoke("move_session_to_project", { sessionId, projectId });
    return;
  }
  const res = await fetchJson(`${BASE}/${sessionId}/move`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ project_id: projectId }),
  });
  if (!res.ok) throw new Error(`Failed to move session: ${res.status}`);
}
