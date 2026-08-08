import { chat } from "./chat";
import { get, writable } from "svelte/store";
import { sessions } from "./sessions";
import { approval } from "./approval";
import type { SSEEvent } from "./types";
import { listen } from "@tauri-apps/api/event";
import { isTauri, tauriInvoke } from "../api/tauri";

export interface HeartbeatStatus {
  connected: boolean;
  lastPing: number;
  sessions: number;
  runningAgents: number;
  scheduler: boolean;
}

export const heartbeat = writable<HeartbeatStatus>({
  connected: false,
  lastPing: 0,
  sessions: 0,
  runningAgents: 0,
  scheduler: false,
});

/** Event types whose data field is a JSON-encoded object (not plain text) */
const OBJECT_EVENTS = new Set(["tool_call", "tool_result", "done", "model_info", "protocol_v1", "approval_needed", "ask_user_pending"]);

/** Try to JSON-parse a string; return parsed value on success, original on failure */
function tryParseJSON(s: string): unknown {
  try {
    return JSON.parse(s);
  } catch {
    return s;
  }
}

let eventSource: EventSource | null = null;
let reconnectAttempts = 0;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let healthCheckTimer: ReturnType<typeof setInterval> | null = null;
let tauriUnlisten: (() => void) | null = null;
const MAX_RECONNECT_DELAY = 30000; // 30s max
const BASE_RECONNECT_DELAY = 1000; // 1s initial
const HEALTH_CHECK_INTERVAL = 30000; // 30s

function getReconnectDelay(): number {
  const delay = Math.min(
    BASE_RECONNECT_DELAY * Math.pow(2, reconnectAttempts),
    MAX_RECONNECT_DELAY,
  );
  // Add jitter (±20%)
  return delay * (0.8 + Math.random() * 0.4);
}

function handleTauriEvent(raw: { session_id?: string; event_type?: string; data?: unknown }) {
  // Filter by current session
  const currentId = get(sessions).currentId;
  if (raw.session_id && currentId && raw.session_id !== currentId) {
    return;
  }
  if (raw.event_type) {
    const eventType = raw.event_type as SSEEvent["type"];
    let eventData: unknown = raw.data;
    if (typeof eventData === "string" && OBJECT_EVENTS.has(eventType)) {
      eventData = tryParseJSON(eventData);
    } else if (
      typeof eventData === "object" &&
      eventData !== null &&
      "data" in (eventData as Record<string, unknown>)
    ) {
      // Tauri events arrive as the full SseEvent object — the .data field
      // is a JSON string. Pull it out and parse if needed.
      const inner = (eventData as { data: string | object }).data;
      eventData = typeof inner === "string" && OBJECT_EVENTS.has(eventType)
        ? tryParseJSON(inner)
        : inner;
    }
    const normalized: SSEEvent = {
      type: eventType,
      data: eventData as never,
    } as SSEEvent;
    if (eventType === "approval_needed") {
      routeApproval(normalized, raw.session_id);
    } else {
      chat.handleSSEEvent(normalized);
    }
  }
}

function handleSSEEvent(event: MessageEvent) {
  try {
    const raw = JSON.parse(event.data) as {
      session_id?: string;
      event_type?: string;
      data?: string;
    };

    // Filter events by current session to avoid cross-session interference
    const currentId = get(sessions).currentId;
    if (raw.session_id && currentId && raw.session_id !== currentId) {
      return;
    }

    if (raw.event_type && raw.data !== undefined) {
      const eventType = raw.event_type as SSEEvent["type"];
      const eventData = OBJECT_EVENTS.has(eventType)
        ? tryParseJSON(raw.data)
        : raw.data;

      const normalized: SSEEvent = {
        type: eventType,
        data: eventData as never,
      } as SSEEvent;
      if (eventType === "approval_needed") {
        routeApproval(normalized, raw.session_id);
      } else {
        chat.handleSSEEvent(normalized);
      }
    } else {
      chat.handleSSEEvent(raw as unknown as SSEEvent);
    }
  } catch {
    // Ignore malformed events
  }
}

async function connectTauri() {
  if (tauriUnlisten) {
    tauriUnlisten();
    tauriUnlisten = null;
  }
  try {
    tauriUnlisten = await listen<{ session_id?: string; event_type?: string; data?: unknown }>("sse_event", (e) => {
      handleTauriEvent(e.payload);
    });
  } catch (err) {
    console.warn("Failed to listen for Tauri sse_event:", err);
  }
}

function connectHttp() {
  if (eventSource) {
    eventSource.close();
    eventSource = null;
  }

  eventSource = new EventSource("/api/events");

  eventSource.onmessage = handleSSEEvent;

  eventSource.onerror = () => {
    // EventSource will auto-reconnect, but we track the state
    if (eventSource) {
      if (eventSource.readyState === EventSource.CLOSED) {
        // Connection fully closed — schedule manual reconnect
        scheduleReconnect();
      }
    }
  };

  eventSource.onopen = () => {
    // Successfully connected (or reconnected)
    reconnectAttempts = 0;
  };
}

function connect() {
  if (isTauri()) {
    void connectTauri();
  } else {
    connectHttp();
  }
}

function scheduleReconnect() {
  if (reconnectTimer) {
    clearTimeout(reconnectTimer);
  }
  const delay = getReconnectDelay();
  reconnectAttempts++;
  reconnectTimer = setTimeout(() => {
    connect();
  }, delay);
}

async function checkHealth(): Promise<boolean> {
  if (isTauri()) {
    try {
      const result = await tauriInvoke("ping") as HeartbeatStatus;
      heartbeat.set({ ...result, connected: true, lastPing: Date.now() });
      return true;
    } catch {
      heartbeat.set({ connected: false, lastPing: Date.now(), sessions: 0, runningAgents: 0, scheduler: false });
      return false;
    }
  }
  try {
    const res = await fetch("/api/health");
    return res.ok;
  } catch {
    return false;
  }
}

function startHealthCheck() {
  if (healthCheckTimer) {
    clearInterval(healthCheckTimer);
  }
  checkHealth(); // fire immediately for initial status
  healthCheckTimer = setInterval(async () => {
    const healthy = await checkHealth();
    if (!healthy) {
      // Server might be restarting — try to reconnect SSE
      if (eventSource) {
        eventSource.close();
        eventSource = null;
      }
      scheduleReconnect();
    }
  }, HEALTH_CHECK_INTERVAL);
}

function cleanup() {
  if (reconnectTimer) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  if (healthCheckTimer) {
    clearInterval(healthCheckTimer);
    healthCheckTimer = null;
  }
  if (eventSource) {
    eventSource.close();
    eventSource = null;
  }
  if (tauriUnlisten) {
    tauriUnlisten();
    tauriUnlisten = null;
  }
  reconnectAttempts = 0;
}

/// Route an approval_needed event to the approval store.
function routeApproval(event: SSEEvent, sessionId?: string) {
  const data = event.data as Record<string, unknown>;
  if (!data || !data.request_id) return;

  approval.push({
    requestId: data.request_id as string,
    sessionId: (sessionId || data.session_id || "") as string,
    toolName: (data.tool_name as string) || "unknown",
    pattern: (data.pattern as string) || "unknown",
    arguments: data.arguments,
    approvedCount: (data.approved_count as number) || 0,
    currentLevel: (data.current_level as string) || "AlwaysAsk",
  });
}

export function connectSSE(): () => void {
  // Clean up any existing connection first
  cleanup();

  connect();
  startHealthCheck();

  // Return cleanup function
  return () => {
    cleanup();
  };
}
