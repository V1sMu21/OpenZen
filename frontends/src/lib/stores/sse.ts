import { chat } from "./chat";
import { get, writable } from "svelte/store";
import { sessions } from "./sessions";
import { approval } from "./approval";
import { markSessionFinished, markSessionRunning, markSessionWaiting } from "./running";
import type { SSEEvent } from "./types";
import type { ProtocolV1Event } from "./parts";
import { listen } from "@tauri-apps/api/event";
import { isTauri, tauriInvoke } from "../api/tauri";

export interface HeartbeatStatus {
  connected: boolean;
  lastPing: number;
  sessions: number;
  runningAgents: number;
  scheduler: boolean;
  /** Last ping failure reason (IPC error text) — surfaced in the health
   *  tag so a red 后端 light is diagnosable without devtools. */
  lastError?: string;
}

export const heartbeat = writable<HeartbeatStatus>({
  connected: false,
  lastPing: 0,
  sessions: 0,
  runningAgents: 0,
  scheduler: false,
});

/** Event types whose data field is a JSON-encoded object (not plain text) */
const OBJECT_EVENTS = new Set(["tool_call", "tool_result", "done", "model_info", "protocol_v1", "protocol_v1_batch", "approval_needed", "ask_user_pending", "reminder_fired"]);

/** Try to JSON-parse a string; return parsed value on success, original on failure */
function tryParseJSON(s: string): unknown {
  try {
    return JSON.parse(s);
  } catch {
    return s;
  }
}

/** Unwrap a protocol_v1 payload into the bare protocol event. The SSE path
 *  sends the event as a JSON string; the Tauri path wraps it in the full
 *  SseEvent object whose `data` field holds that string. */
export function protocolEventFrom(rawData: unknown): ProtocolV1Event | null {
  let inner: unknown = rawData;
  if (inner && typeof inner === "object" && "data" in (inner as Record<string, unknown>)) {
    inner = (inner as { data: unknown }).data;
  }
  if (typeof inner === "string") inner = tryParseJSON(inner);
  if (inner && typeof inner === "object") return inner as ProtocolV1Event;
  return null;
}

/** Expand a `protocol_v1` or `protocol_v1_batch` payload into the ordered
 *  list of protocol events it carries. The backend batches a burst of events
 *  into one SSE message; order must be preserved. */
export function protocolEventsFrom(rawData: unknown): ProtocolV1Event[] {
  let inner: unknown = rawData;
  if (inner && typeof inner === "object" && "data" in (inner as Record<string, unknown>)) {
    inner = (inner as { data: unknown }).data;
  }
  if (typeof inner === "string") inner = tryParseJSON(inner);
  if (!inner || typeof inner !== "object") return [];
  const batch = (inner as { events?: unknown }).events;
  if (Array.isArray(batch)) {
    return batch.filter((e): e is ProtocolV1Event => !!e && typeof e === "object");
  }
  // A single protocol_v1 payload is its own event object.
  return [inner as ProtocolV1Event];
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

/** Track live agent runs for the sidebar indicator. Must see ALL sessions'
 *  events (background included), so call it before the current-session
 *  filter in both dispatch paths. Run start is signaled by model_info
 *  (broadcast before the loop) and run activity; done/error end the run. */
const RUN_START_EVENTS = new Set(["model_info", "token", "thinking", "tool_call", "tool_result", "protocol_v1", "protocol_v1_batch"]);
const RUN_END_EVENTS = new Set(["done", "error"]);

function trackSessionState(sessionId: string | undefined, event: ProtocolV1Event) {
  if (!sessionId) return;
  if (event.type === "ask_user_pending") {
    markSessionWaiting(sessionId);
  } else if (event.type === "ask_user_answered") {
    markSessionRunning(sessionId);
  }
}

function trackRunState(sessionId: string | undefined, eventType: string | undefined) {
  if (!sessionId || !eventType) return;
  if (RUN_START_EVENTS.has(eventType)) {
    markSessionRunning(sessionId);
  } else if (RUN_END_EVENTS.has(eventType)) {
    markSessionFinished(sessionId);
  }
}

function handleTauriEvent(raw: { session_id?: string; event_type?: string; data?: unknown }) {
  trackRunState(raw.session_id, raw.event_type);
  // Strict session guard: an event belonging to another session must NEVER
  // reach chat.handleSSEEvent. The old check required a current session to be
  // selected already, so an event arriving in the window between
  // `sessions.select(id)` and `loadSession()` resolving was painted into the
  // PREVIOUS session's bubble (cross-session bleed).
  const currentId = get(sessions).currentId;
  const isBackground = !!raw.session_id && raw.session_id !== currentId;
  if (isBackground) {
    const sid = raw.session_id as string;
    // A background session finishing is still important: without this,
    // switching back would resurrect its pre-done streaming snapshot
    // from the processing cache (T3.6 stale-stream fix).
    if (raw.event_type === "done" || raw.event_type === "error") {
      chat.noteSessionFinished(sid);
    } else if (raw.event_type === "protocol_v1" || raw.event_type === "protocol_v1_batch") {
      // Keep the background run's cached snapshot current so switching back
      // shows the cards produced while the user was elsewhere.
      for (const protoEvent of protocolEventsFrom(raw.data)) {
        trackSessionState(sid, protoEvent);
        chat.applyBackgroundProtocolEvent(sid, protoEvent);
      }
    }
    return;
  }
  if (raw.event_type) {
    const eventType = raw.event_type as SSEEvent["type"];
    if (eventType === "protocol_v1" || eventType === "protocol_v1_batch") {
      // Single payload or a backend batch: apply every event, in order.
      const events = protocolEventsFrom(raw.data);
      if (events.length === 0) {
        console.warn("[sse] dropped unparsable protocol payload", raw.event_type);
        return;
      }
      for (const protoEvent of events) {
        trackSessionState(raw.session_id, protoEvent);
        chat.handleSSEEvent({ type: "protocol_v1", data: protoEvent } as SSEEvent);
      }
      return;
    }
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

    trackRunState(raw.session_id, raw.event_type);

    // Filter events by current session to avoid cross-session interference
    const currentId = get(sessions).currentId;
    if (raw.session_id && currentId && raw.session_id !== currentId) {
      if (raw.event_type === "done" || raw.event_type === "error") {
        chat.noteSessionFinished(raw.session_id);
      } else if (
        raw.event_type === "protocol_v1" ||
        raw.event_type === "protocol_v1_batch"
      ) {
        for (const protoEvent of protocolEventsFrom(raw.data)) {
          trackSessionState(raw.session_id, protoEvent);
          chat.applyBackgroundProtocolEvent(raw.session_id, protoEvent);
        }
      }
      return;
    }

    if (raw.event_type === "protocol_v1" || raw.event_type === "protocol_v1_batch") {
      // A batch envelope carries several events in `data.events`; expand it
      // and apply them IN ORDER. This HTTP/EventSource path is separate from
      // the Tauri one, and only the Tauri path used to expand batches — so
      // the day `oz-server` starts batching, every streamed event in the
      // browser would vanish without an error (review finding B-6).
      const events = protocolEventsFrom(raw.data);
      if (events.length === 0) {
        console.warn("[sse] dropped unparsable protocol payload", raw.event_type);
        return;
      }
      for (const protoEvent of events) {
        trackSessionState(raw.session_id, protoEvent);
        chat.handleSSEEvent({ type: "protocol_v1", data: protoEvent } as SSEEvent);
      }
    } else if (raw.event_type && raw.data !== undefined) {
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
      void tauriInvoke("log_frontend", { line: "ping:start", level: "debug" }).catch(() => {});
      const result = await tauriInvoke("ping") as HeartbeatStatus;
      void tauriInvoke("log_frontend", { line: "ping:ok", level: "debug" }).catch(() => {});
      heartbeat.set({ ...result, connected: true, lastPing: Date.now(), lastError: undefined });
      return true;
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      void tauriInvoke("log_frontend", { line: `ping:fail: ${msg}`, level: "warn" }).catch(() => {});
      heartbeat.set({
        connected: false,
        lastPing: Date.now(),
        sessions: 0,
        runningAgents: 0,
        scheduler: false,
        lastError: msg,
      });
      return false;
    }
  }
  try {
    const res = await fetch("/api/health");
    // Mirror the Tauri branch: without writing the store, the backend
    // indicator stayed red forever in HTTP mode.
    heartbeat.update((h) => ({
      ...h,
      connected: res.ok,
      lastPing: Date.now(),
    }));
    return res.ok;
  } catch {
    heartbeat.update((h) => ({ ...h, connected: false, lastPing: Date.now() }));
    return false;
  }
}

function startHealthCheck() {
  if (healthCheckTimer) {
    clearInterval(healthCheckTimer);
  }
  checkHealth(); // fire immediately for initial status
  healthCheckTimer = setInterval(async () => {
    // Background session windows skip health pings; the next tick after
    // the window becomes visible covers it (P3/A9).
    if (document.hidden) return;
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
    origin: (data.origin as string) || undefined,
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
