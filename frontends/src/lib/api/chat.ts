import { isTauri, tauriInvoke } from "./tauri";
import { requestAuthToken } from "../stores/auth";

export interface ChatResponse {
  session_id: string;
  status: string;
  response?: string | null;
  exit_reason?: string | null;
}

export interface ModelEntry {
  name: string;
  model: string;
  provider: string;
  context_win: number;
}

let cachedAuthToken: string | null = null;

export function setAuthToken(token: string) {
  cachedAuthToken = token;
  try { localStorage.setItem("openzen_auth_token", token); } catch { /* ignore */ }
}

export function getAuthToken(): string | null {
  if (cachedAuthToken) return cachedAuthToken;
  try { cachedAuthToken = localStorage.getItem("openzen_auth_token"); } catch { /* ignore */ }
  if (!cachedAuthToken) {
    const p = new URLSearchParams(window.location.search);
    cachedAuthToken = p.get("token");
  }
  return cachedAuthToken;
}

/** Try to discover the current server auth token from the unauthenticated
 *  /api/health endpoint, which now returns it in the response body. */
async function discoverAuthToken(): Promise<string | null> {
  try {
    const res = await fetch("/api/health");
    if (res.ok) {
      const data = await res.json();
      return data.auth_token || null;
    }
  } catch { /* network errors — ignore */ }
  return null;
}

export async function fetchJson(path: string, init?: RequestInit): Promise<Response> {
  const headers: Record<string, string> = {
    ...(init?.headers as Record<string, string> || {}),
  };
  const token = getAuthToken();
  if (token) headers["Authorization"] = `Bearer ${token}`;
  const merged = { ...init, headers };
  const res = await fetch(path, merged);
  if (res.status === 401 && path !== "/api/health") {
    setAuthToken("");
    // First try to auto-discover the current server token from /api/health.
    const serverToken = await discoverAuthToken();
    if (serverToken) {
      setAuthToken(serverToken);
      const retryRes = await fetch(path, {
        ...merged,
        headers: { ...headers, "Authorization": `Bearer ${serverToken}` },
      });
      return retryRes;
    }
    // Fall back to showing the auth dialog for manual entry.
    const tokenInput = await requestAuthToken();
    if (tokenInput) {
      setAuthToken(tokenInput.trim());
      const retryRes = await fetch(path, {
        ...merged,
        headers: { ...headers, "Authorization": `Bearer ${tokenInput.trim()}` },
      });
      return retryRes;
    }
  }
  return res;
}

export async function listModels(): Promise<ModelEntry[]> {
  if (isTauri()) {
    try {
      return (await tauriInvoke("list_models")) as ModelEntry[];
    } catch (e) {
      console.error("[listModels] Tauri invoke failed:", e);
      return [];
    }
  }
  try {
    const res = await fetchJson("/api/models");
    if (!res.ok) return [];
    return await res.json();
  } catch (e) {
    console.error("[listModels] HTTP fetch failed:", e);
    return [];
  }
}

export async function sendMessage(
  message: string,
  sessionId: string,
  sessionName?: string,
  modelName?: string,
): Promise<ChatResponse> {
  if (isTauri()) {
    await tauriInvoke("send_message", {
      message,
      sessionId,
      sessionName: sessionName || null,
      modelName: modelName || null,
    });
    return { session_id: sessionId, status: "completed" };
  }
  const res = await fetchJson("/api/chat", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      message,
      session_id: sessionId,
      session_name: sessionName,
      model_name: modelName || null,
    }),
  });

  if (!res.ok) {
    let detail = `${res.status} ${res.statusText}`;
    try {
      const txt = await res.text();
      if (txt) detail += ` — ${txt.slice(0, 200)}`;
    } catch {
      // ignore — we already have a status code
    }
    throw new Error(`Chat request failed: ${detail}`);
  }

  try {
    await res.body?.cancel();
  } catch {
    // cancel can throw on an already-closed socket; that's fine
  }

  return { session_id: sessionId, status: "completed" };
}
