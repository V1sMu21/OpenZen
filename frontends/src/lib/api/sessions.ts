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

export async function listSessions(projectId?: string): Promise<SessionInfo[]> {
  if (isTauri()) {
    return (await tauriInvoke("list_sessions", { projectId: projectId || null })) ?? [];
  }
  const url = projectId ? `${BASE}?project_id=${encodeURIComponent(projectId)}` : BASE;
  const res = await fetchJson(url);
  if (!res.ok) throw new Error(`Failed to list sessions: ${res.status}`);
  const data = await res.json();
  return data.sessions ?? data ?? [];
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

export async function getSession(id: string): Promise<any> {
  if (isTauri()) {
    return await tauriInvoke("get_session", { id });
  }
  const res = await fetchJson(`${BASE}/${id}`);
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
