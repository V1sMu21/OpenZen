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
  /** Derived protocol family ("openai"/"claude"), from the entry name. */
  provider: string;
  /** [providers.<id>] this entry borrows apibase/apikey from, if any. */
  provider_id?: string | null;
  context_win: number;
  /** Declared input modalities; always non-empty (["text"] fallback). */
  modalities?: string[];
  is_local?: boolean;
  /** True when mykey.toml default_session points here (list_models only). */
  is_default?: boolean;
}

/** A named apibase+apikey credential shared by multiple model entries. */
export interface ProviderEntry {
  id: string;
  /** Full base URL — shown in the settings UI, not a secret. */
  apibase: string;
  /** Masked key hint; the literal key never leaves the config. */
  key_hint: string;
  model_count: number;
}

export interface SkillMcpItem {
  name: string;
  description: string;
  active: boolean;
  quality?: number;
  successCount?: number;
  failureCount?: number;
}

export interface McpServerItem {
  name: string;
  command: string;
  enabled: boolean;
  autoStart?: boolean;
}

export interface TokenStats {
  totals: { in: number; out: number };
  perDay: { day: string; in: number; out: number }[];
  perModel: { model: string; in: number; out: number }[];
  perSession: {
    id: string;
    name: string;
    createdAt: string;
    messageCount: number;
    tokensIn: number;
    tokensOut: number;
  }[];
}

export interface ModelUpsertArgs {
  name: string;
  /** When set, the entry borrows apibase/apikey from this provider and
   *  inline apibase/apikey are ignored/stripped. Null/omitted = standalone. */
  provider?: string | null;
  /** Blank/omitted on an edit keeps the stored value; required for new
   *  standalone entries. */
  apibase?: string;
  apikey?: string;
  model?: string;
  context_win: number;
  /** Input modalities; defaults to ["text"] server-side. */
  modalities?: string[];
}

export interface ProviderUpsertArgs {
  id: string;
  /** Required (validated server-side). */
  apibase: string;
  /** Blank/omitted on an edit keeps the stored key. */
  apikey?: string;
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
