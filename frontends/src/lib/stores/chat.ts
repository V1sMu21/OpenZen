import { writable, get } from "svelte/store";
import type { Message, SSEEvent, ModelInfo } from "./types";
import type { ModelEntry } from "../api/chat";
import { estimateTokens, formatTokenCount } from "./types";
import type { UIMessagePart, ProtocolV1Event, ToolInvocationPart, DataPart } from "./parts";
import { sidepanel } from "./sidepanel.svelte";
import { convertStreamEventsToParts, generatePartId } from "./parts";
import { applyProtocolEvent, clearReasoningTimers } from "./protocol-processor";
import { sessions } from "./sessions";
import { isTauri, tauriInvoke } from "../api/tauri";
import { sendMessage as httpSendMessage } from "../api/chat";
import { stopSession, type SessionPageData } from "../api/sessions";

export interface PendingAskUser {
  question: string;
  candidates: string[];
  askId?: string;
}

export interface Attachment {
  id: string;
  path: string;
  name: string;
  type: "file" | "image";
}

/** A scheduled or repeating reminder scheduled by the agent. */
export interface ReminderTask {
  /** tool call id (dedup key) */
  id: string;
  /** The reminder message (card title) */
  title: string;
  /** Seconds until the first fire */
  delaySeconds: number;
  /** Total repeat count (0 = one-shot; >0 = heartbeat task) */
  repeatCount: number;
  /** Remaining repeats (updated by reminder_fired SSE events) */
  remaining: number;
  /** Seconds between repeats */
  repeatIntervalSecs: number;
  /** When the next fire happens (ms epoch) */
  fireAtMs: number;
  /** active until all repeats fired, then done */
  status: "active" | "done";
}

// ── Streaming event coalescing ──
// text_delta / reasoning_delta arrive at token frequency from SSE; each
// one used to trigger a full store update (streamingParts copy + all
// $derived re-evaluations across every ChatMessage). We accumulate the
// render-only protocol events and apply them in ONE update() on the next
// animation frame instead. Side-effect events (ask_user, todo, context
// usage, artifact) are handled immediately, never coalesced.
const RENDER_ONLY_EVENTS = new Set([
  "reasoning_start", "reasoning_delta", "reasoning_end",
  "text_start", "text_delta", "text_end",
  "tool_input_start", "tool_input_delta", "tool_input_available",
  "tool_output_available",
]);

export interface ChatState {
  messages: Message[];
  isProcessing: boolean;
  error: string | null;
  modelInfo: ModelInfo | null;
  /** Currently selected model session name (e.g. "local", "local-qwen27b") */
  selectedModel: string | null;
  /** Available models from the backend */
  modelList: ModelEntry[];
  /** Whether the model switcher dialog is visible */
  showModelSwitcher: boolean;
  /** The parts of the currently streaming assistant message.
   *  During streaming, the last mutable part may have state='streaming'. */
  streamingParts: UIMessagePart[];
  /**
   * When the agent calls `ask_user`, the loop exits with should_exit and
   * the result payload carries the question + candidate answers. We capture
   * it here so the UI can pop a dialog and let the user respond.
   */
   pendingAskUser: PendingAskUser | null;
    /** File and image attachments for the next message. */
    attachments: Attachment[];
    /** Todo items from the agent's todowrite/todoupdate tools. */
   todos: Array<{id:string;content:string;status:string;priority:string;order:number}>;
   /** Scheduled/heartbeat reminders from the schedule_reminder tool.
    *  Parsed from tool invocations in the message stream; rendered as a
    *  card under the todo rail. */
   reminders: ReminderTask[];
   compressionNotice: string | null;
   cumulativeInputTokens: number;
   cumulativeOutputTokens: number;
   /** Pagination window: true when older persisted messages are available. */
   hasMoreMessages?: boolean;
   /** True while a "load earlier" page request is in flight. */
   loadingEarlier?: boolean;
   /** True while a session page is loading: keep the previous list visible. */
   loadingSession?: boolean;
}

// 30 minutes: long enough for a multi-step task with many slow
// tool calls, and the watchdog is *reset* on every incoming token,
// so this only fires if the server truly stops talking to us.
const PROCESSING_TIMEOUT_MS = 30 * 60 * 1000;

/** Parse scheduled/heartbeat reminders from schedule_reminder tool
 *  invocations in the message stream (dedup by tool call id). */
function scanRemindersFromMessages(messages: Message[]): ReminderTask[] {
  const out: ReminderTask[] = [];
  for (const m of messages) {
    if (m.role !== "assistant") continue;
    for (const p of m.parts ?? []) {
      if (p.type !== "tool-invocation" || p.name !== "schedule_reminder") continue;
      let args: Record<string, unknown> = {};
      try { args = JSON.parse(p.args); } catch { continue; }
      const title = typeof args.message === "string" ? args.message : "";
      if (!title) continue;
      const delay = typeof args.delay_seconds === "number" ? args.delay_seconds : 60;
      const repeat = typeof args.repeat_count === "number" ? args.repeat_count : 0;
      const interval = typeof args.repeat_interval_seconds === "number"
        ? args.repeat_interval_seconds : delay;
      // Prefer the real fire_at_ms from the tool result; fall back to
      // now + delay when the result is unavailable (e.g. after reload).
      let fireAtMs = Date.now() + delay * 1000;
      if (p.result) {
        try {
          const r = JSON.parse(p.result);
          if (typeof r.fire_at_ms === "number") fireAtMs = r.fire_at_ms;
          else if (r && typeof r === "object" && "data" in r) {
            const inner = (r as { data?: { fire_at_ms?: number } }).data;
            if (typeof inner?.fire_at_ms === "number") fireAtMs = inner.fire_at_ms;
          }
        } catch { /* keep the estimate */ }
      }
      out.push({
        id: p.toolCallId,
        title,
        delaySeconds: delay,
        repeatCount: repeat,
        remaining: repeat,
        repeatIntervalSecs: interval,
        fireAtMs,
        status: "active",
      });
    }
  }
  return out;
}

function createChatStore() {
  const { subscribe, set, update } = writable<ChatState>({
    messages: [],
    isProcessing: false,
    error: null,
    modelInfo: null,
    selectedModel: null,
    modelList: [],
     showModelSwitcher: false,
     streamingParts: [],
     pendingAskUser: null,
      attachments: [],
      todos: [],
      reminders: [],
     compressionNotice: null,
     cumulativeInputTokens: 0,
     cumulativeOutputTokens: 0,
   });

  // Per-session state cache — preserves in-progress agent output when
  // the user switches to another session and back. Without this cache,
  // loadSession() wipes streamingParts + the unpersisted assistant
  // message, so the bubble disappears. The cache is evicted when the
  // agent finishes (done event) or the user sends a new message.
   const sessionCache = new Map<string, {
    messages: Message[];
    streamingParts: UIMessagePart[];
    isProcessing: boolean;
    pendingAskUser: PendingAskUser | null;
    attachments: Attachment[];
    todos: ChatState['todos'];
    compressionNotice: string | null;
    modelInfo: ModelInfo | null;
  }>();

  // ── Idle-session view cache (T3.6) ──────────────────────────────
  // Bounded LRU of the last SESSION_CACHE_MAX fully-loaded session pages.
  // Switching back to a recent idle session paints synchronously from this
  // cache (<100ms) instead of waiting on IPC/HTTP + JSON conversion.
  const SESSION_PAGE_SIZE = 200;
  const SESSION_VIEW_CACHE_MAX = 4;
  const SESSION_VIEW_CACHE_TTL_MS = 60 * 60 * 1000; // 1h

  interface SessionViewCacheEntry {
    at: number;
    state: {
      messages: Message[];
      /** True when the backend reports the session still Running — restored
       *  on switch-back so live streaming keeps rendering and the title-bar
       *  pill doesn't flip to 完成 while a long task is in flight. */
      isProcessing: boolean;
      todos: ChatState['todos'];
      reminders: ReminderTask[];
      pendingAskUser: PendingAskUser | null;
      modelInfo: ModelInfo | null;
      hasMoreMessages: boolean;
    };
  }

  const sessionViewCache = new Map<string, SessionViewCacheEntry>();

  /** Raw-message skip-from-end for the next "load earlier" page. */
  const sessionPageState = new Map<string, { offset: number; hasMore: boolean }>();

  function touchSessionViewCache(sessionId: string) {
    const entry = sessionViewCache.get(sessionId);
    if (!entry) return;
    sessionViewCache.delete(sessionId);
    sessionViewCache.set(sessionId, entry); // refresh LRU insertion order
  }

  function storeSessionViewCache(
    sessionId: string,
    state: SessionViewCacheEntry['state'],
  ) {
    sessionViewCache.set(sessionId, { at: Date.now(), state });
    while (sessionViewCache.size > SESSION_VIEW_CACHE_MAX) {
      const oldestKey = sessionViewCache.keys().next().value;
      if (oldestKey === undefined) break;
      sessionViewCache.delete(oldestKey);
    }
  }

  // Monotonic sequence for loadSession calls: a stale getSession response
  // from a previously-selected session must never overwrite the state of
  // the session the user switched to in the meantime.
  let loadSeq = 0;

  /** Synchronously read the current chat state (subscribe fire-and-read). */
  function readState(): ChatState {
    let state: ChatState | null = null;
    const unsub = subscribe((s) => { state = s; });
    unsub();
    return state!;
  }

  type ServerSessionMessage = {
    idx?: number;
    role?: string;
    content?: string;
    thinking?: string;
    timestamp?: string;
    toolCalls?: Array<{ name: string; arguments: string; result?: string }>;
    tool_results?: unknown[];
    streamEvents?: unknown[];
    parts?: UIMessagePart[];
    duration?: number;
    tokensIn?: number;
    tokensOut?: number;
    contextTokens?: number;
    modelInfo?: { model: string; provider: string; contextWindow: number; isLocal: boolean };
    exitReason?: string;
    exit_reason?: string;
    children?: string[];
  };

  function parseSessionMessages(sessionId: string, raw: ServerSessionMessage[]): Message[] {
    const messages: Message[] = raw
      .filter((m) => {
        const hasText = (m.content?.trim()?.length ?? 0) > 0;
        const hasToolResults = (m.tool_results?.length ?? 0) > 0;
        const hasStreamEvents = (m.streamEvents?.length ?? 0) > 0;
        // User messages with only tool_results (no text) are internal
        // protocol carriers; skip them in the chat UI.
        if (m.role === "user" && !hasText && hasToolResults) return false;
        return hasText || hasToolResults || hasStreamEvents;
      })
      .map((m, pageIdx) => {
        let parts: UIMessagePart[] | undefined;
        if (m.role === "assistant") {
          if (m.parts && m.parts.length > 0) {
            parts = m.parts;
          } else if (m.streamEvents && m.streamEvents.length > 0) {
            parts = convertStreamEventsToParts(m.streamEvents as import("./types").StreamEventItem[]);
          }
        }
        // Prefer the server-assigned global idx. It keeps Svelte keys
        // stable when older pages are prepended later.
        const globalIdx = typeof m.idx === 'number' ? m.idx : pageIdx;
        return {
          id: `${sessionId}-msg-${globalIdx}`,
          role: (m.role as Message["role"]) || "user",
          content: m.content || "",
          thinking: m.thinking,
          timestamp: m.timestamp || new Date().toISOString(),
          toolCalls: m.toolCalls as Message["toolCalls"],
          // Parts are the converted form of streamEvents — keeping both
          // roughly doubles per-message memory in long sessions.
          // ChatMessage reads streamEvents only as a fallback when
          // parts are absent.
          streamEvents:
            parts && parts.length > 0
              ? undefined
              : (m.streamEvents as Message["streamEvents"]),
          parts,
          streaming: false,
          duration: m.duration,
          tokensIn: m.tokensIn,
          tokensOut: m.tokensOut,
          contextTokens: m.contextTokens,
          modelInfo: m.modelInfo,
          exitReason: m.exitReason ?? m.exit_reason,
          children: m.children ?? [],
        };
      });

    // Post-process: convert intervention user messages into cards inside
    // the preceding assistant message for restart-consistency.
    for (let i = messages.length - 1; i >= 1; i--) {
      const msg = messages[i];
      if (msg.role === "user" && msg.content?.startsWith("[USER INTERVENTION")) {
        for (let j = i - 1; j >= 0; j--) {
          if (messages[j].role === "assistant") {
            const parts = messages[j].parts ?? [];
            const cleanContent = msg.content!.replace(/^\[USER INTERVENTION.*?\n/, "");
            parts.push({
              type: "data",
              id: `intervention_${messages[j].id}_${i}`,
              dataType: "user_intervention",
              content: cleanContent,
              transient: false,
            } as UIMessagePart);
            messages[j] = { ...messages[j], parts };
            break;
          }
        }
        messages.splice(i, 1);
      }
    }
    return messages;
  }

  function findPendingAskUserIn(messages: Message[]): PendingAskUser | null {
    for (let i = messages.length - 1; i >= 0; i--) {
      const m = messages[i];
      if (m.role !== "assistant") continue;
      const parts = m.parts ?? [];
      for (let j = parts.length - 1; j >= 0; j--) {
        const p = parts[j];
        if (p.type !== "tool-invocation") continue;
        if (p.name !== "ask_user") continue;
        const raw = p.result ?? "";
        if (!raw) continue;
        try {
          const parsed = JSON.parse(raw);
          const data = parsed?.data;
          if (parsed?.intent !== "HUMAN_INTERVENTION" && parsed?.status !== "INTERRUPT") {
            continue;
          }
          if (data && typeof data.question === "string") {
            return {
              question: data.question,
              candidates: Array.isArray(data.candidates) ? data.candidates : [],
            };
          }
        } catch {
          // not JSON, ignore
        }
      }
      // Only consider the latest assistant turn.
      break;
    }
    return null;
  }

  function setViewState(sessionId: string, state: SessionViewCacheEntry['state']) {
    set({
      messages: state.messages,
      isProcessing: state.isProcessing,
      error: null,
      modelInfo: state.modelInfo,
      selectedModel: null,
      modelList: [],
      showModelSwitcher: false,
      streamingParts: [],
      pendingAskUser: state.pendingAskUser,
      attachments: [],
      todos: state.todos,
      reminders: state.reminders,
      compressionNotice: null,
      cumulativeInputTokens: 0,
      cumulativeOutputTokens: 0,
      hasMoreMessages: state.hasMoreMessages,
      loadingEarlier: false,
      loadingSession: false,
    });
    // A restored Running session must hold the processing watchdog open;
    // otherwise the 30-min timer never arms and the live pill state and
    // the streaming bubble live off a stale flag.
    if (state.isProcessing) startProcessingWatchdog();
  }

  async function applySessionPage(
    sessionId: string,
    data: SessionPageData,
    prepend: boolean,
    seq: number,
  ) {
    const rawMessages = (data.messages ?? []) as ServerSessionMessage[];
    const messages = parseSessionMessages(sessionId, rawMessages);
    const serverTodos = (data.todos ?? []) as ChatState['todos'];
    const hasMore = data.has_more === true;
    const offset = typeof data.offset === 'number' ? data.offset : 0;
    // The backend reports the session's live status with the page. A
    // long-running task survives page reloads and session switches; without
    // this restore the UI painted 完成状态 and dropped every streaming event
    // (no live message to render into) while the agent was still working.
    const serverRunning = /^running$/i.test(String(data.status ?? ""));
    const current = readState();
    const currentSid = get(sessions).currentId;

    if (prepend) {
      const existingIds = new Set(current.messages.map((m) => m.id));
      const older = messages.filter((m) => !existingIds.has(m.id));
      const merged = [...older, ...current.messages];
      if (seq !== loadSeq || currentSid !== sessionId) {
        // A stale or background prepend must not paint into whichever
        // session is visible now, and we no longer have the target
        // session's cached window to merge with — discard it.
        return;
      }
      set({ ...current, messages: merged, hasMoreMessages: hasMore, loadingEarlier: false });
      storeSessionViewCache(sessionId, {
        messages: merged,
        isProcessing: current.isProcessing,
        todos: current.todos,
        // loadEarlierMessages refuses to run mid-task, so this cache write
        // is always idle-context: the finished task's reminders stay gone.
        reminders: [],
        pendingAskUser: current.pendingAskUser,
        modelInfo: current.modelInfo,
        hasMoreMessages: hasMore,
      });
    } else {
      let pendingAskUser: PendingAskUser | null = null;
      const lastMsg = messages[messages.length - 1];
      if (lastMsg?.role === "assistant" && lastMsg.exitReason === "ASK_USER") {
        pendingAskUser = findPendingAskUserIn(messages);
      }
      // Restore the live Running state: the backend reports the session's
      // status with the page, so a long task survives reloads and session
      // switches. `current.isProcessing` only counts when the current store
      // state actually belongs to THIS session — otherwise a running
      // previous session would leak its flag into the newly selected one.
      const sessionLive = (currentSid === sessionId && current.isProcessing) || serverRunning;
      const viewState = {
        messages,
        isProcessing: sessionLive,
        todos: serverTodos,
        // Only a live/running task owns reminder cards. Scanning them out
        // of the message history for a finished session resurrected
        // "active" cards on every reload even though the task — and the
        // backend's pending entries — are long gone.
        reminders: sessionLive ? scanRemindersFromMessages(messages) : [],
        pendingAskUser,
        modelInfo: null,
        hasMoreMessages: hasMore,
      };
      storeSessionViewCache(sessionId, viewState);
      if (seq !== loadSeq || currentSid !== sessionId) return;
      if (sessionLive && current.messages.length > 0) {
        // An optimistic run started while this response was in flight; the
        // live state is newer than the persisted page. Refresh only the cache.
        return;
      }
      setViewState(sessionId, viewState);
    }

    sessionPageState.set(sessionId, { offset, hasMore });
  }

  /**
   * Wall-clock arrival time of every part, indexed by a stable
   * identity (partId for text/reasoning parts, toolCallId for
   * tool-invocation parts). We use a map so the index is
   * independent of `streamingParts.length` — data_* events push
   * extra parts into streamingParts but never record an arrival,
   * which would otherwise misalign the indices used to compute
   * per-part durations.
   */
  const partArrivalTimes = new Map<string, number>();
  /** For text/reasoning parts, which lack their own id field on
   *  `state` change events, we still want a deterministic arrival
   *  order. The fallback key is the part's *identity* (id for
   *  text/reasoning, toolCallId for tool-invocations). */
  const partArrivalOrder: string[] = [];
  const arrivalKey = (p: { type: string } & Record<string, unknown>): string => {
    if (p.type === "tool-invocation") {
      return `tool:${(p as { toolCallId?: string }).toolCallId ?? ""}`;
    }
    if (p.type === "data") {
      return `data:${(p as { id?: string }).id ?? ""}`;
    }
    return `text:${(p as { id?: string }).id ?? ""}`;
  };

  // ── Streaming event coalescing (see RENDER_ONLY_EVENTS at module
  // top): deltas are batched into ONE store update per animation
  // frame instead of one update per token. Live here (inside the
  // store factory) because flush needs access to partArrivalTimes,
  // update, contentFromParts and the processing watchdog.
  let pendingStreamEvents: ProtocolV1Event[] = [];
  let streamFlushRaf: number | null = null;

  /** Record per-part arrival timestamps for duration computation. */
  function recordArrivalForEvent(protoEvent: ProtocolV1Event) {
    let arrivalKeyForEvent: string | null = null;
    if (protoEvent.type === 'reasoning_start' || protoEvent.type === 'text_start') {
      if (protoEvent.id) arrivalKeyForEvent = `text:${protoEvent.id}`;
    } else if (protoEvent.type === 'tool_input_start') {
      if (protoEvent.tool_call_id) arrivalKeyForEvent = `tool:${protoEvent.tool_call_id}`;
    } else if (protoEvent.type === 'tool_output_available') {
      if (protoEvent.tool_call_id) arrivalKeyForEvent = `tool_output:${protoEvent.tool_call_id}`;
    }
    if (arrivalKeyForEvent !== null) {
      const now = Date.now();
      if (!partArrivalTimes.has(arrivalKeyForEvent)) {
        partArrivalTimes.set(arrivalKeyForEvent, now);
        partArrivalOrder.push(arrivalKeyForEvent);
      }
    }
  }

  /**
   * Guarantee a streaming assistant bubble exists for the in-flight run.
   * A restored Running session (page reload / switch-back) can end its
   * persisted window with a USER message — the run's assistant message is
   * only persisted at after_run. liveMessageId requires a trailing
   * assistant message, so without this the run's streaming events had
   * nowhere to render: the task kept running invisibly and the newest
   * cards never appeared.
   */
  function ensureLiveAssistantMessage() {
    const s = readState();
    if (!s.isProcessing) return;
    const last = s.messages[s.messages.length - 1];
    if (last && last.role === "assistant" && last.streaming) return;
    startAssistantMessageInternal();
  }

  /** Shared by the public startAssistantMessage and the restored-run
   *  fallback above (factory scope — the public methods live on the
   *  returned object and aren't reachable from the event flush path). */
  function startAssistantMessageInternal() {
    const currentSid = get(sessions).currentId;
    if (currentSid) {
      sessionCache.delete(currentSid);
      // The pre-send page is no longer the truth for this session;
      // dropping it prevents a fast switch-back from painting the
      // conversation without the newly-started turn.
      sessionViewCache.delete(currentSid);
    }
    startProcessingWatchdog();
    cancelPendingStreamEvents();
    partArrivalTimes.clear();
    partArrivalOrder.length = 0;
    const msgId = generateId();
    update((s) => {
      // Invariant: exactly ONE live (streaming) assistant bubble exists.
      // Any earlier streaming message is an orphan (e.g. legacy mid-run
      // user-message inserts) — freeze it here or its footer timer ticks
      // forever while the run's output renders into the new bubble.
      const frozenMessages = s.messages.map((m) => {
        if (m.role !== "assistant" || !m.streaming) return m;
        const startedMs = m.timestamp ? new Date(m.timestamp).getTime() : NaN;
        return {
          ...m,
          streaming: false,
          duration: m.duration ?? (Number.isFinite(startedMs) ? Math.max(0, Date.now() - startedMs) : 0),
        };
      });
      return {
        ...s,
        isProcessing: true,
        streamingParts: [],
        pendingAskUser: null,
        messages: [
          ...frozenMessages,
          {
            id: msgId,
            role: "assistant",
            content: "",
            timestamp: new Date().toISOString(),
            streaming: true,
            modelInfo: s.modelInfo ?? undefined,
            children: [],
          },
        ],
      };
    });
  }

  /** Apply all pending render events in a single store update. */
  function flushStreamingEvents() {
    streamFlushRaf = null;
    if (pendingStreamEvents.length === 0) return;
    const batch = pendingStreamEvents;
    pendingStreamEvents = [];
    resetProcessingWatchdog();
    ensureLiveAssistantMessage();
    for (const ev of batch) recordArrivalForEvent(ev);
    update((s) => {
      const parts = [...s.streamingParts];
      for (const ev of batch) applyProtocolEvent(parts, ev);
      return withStreamingParts(s, parts);
    });
  }

  // rAF never fires while the window is hidden/minimized, but Tauri events
  // keep arriving — a 7x24 background agent run would accumulate every
  // stream event in pendingStreamEvents without bound. The interval
  // fallback drains the queue while rAF is stalled; flushing while visible
  // is harmless (the batch is drained atomically and a redundant rAF tick
  // no-ops on the empty queue).
  const STREAM_FLUSH_FALLBACK_MS = 1000;
  const STREAM_FLUSH_QUEUE_LIMIT = 500;
  let streamFlushInterval: ReturnType<typeof setInterval> | null = null;

  function ensureStreamFlushFallback() {
    if (streamFlushInterval !== null) return;
    streamFlushInterval = setInterval(() => {
      if (pendingStreamEvents.length === 0) {
        clearInterval(streamFlushInterval!);
        streamFlushInterval = null;
        return;
      }
      flushStreamingEvents();
    }, STREAM_FLUSH_FALLBACK_MS);
  }

  /** Queue a render-only event; coalesce into the next animation frame. */
  function queueStreamEvent(protoEvent: ProtocolV1Event) {
    pendingStreamEvents.push(protoEvent);
    // Reset the watchdog on every event, not only on flush: when the
    // window is minimized/occluded, rAF stops firing so flushes pause
    // while events keep arriving — the watchdog must not fire then.
    resetProcessingWatchdog();
    // Hard cap: never let the queue grow past the limit even if both rAF
    // and the interval somehow stall.
    if (pendingStreamEvents.length >= STREAM_FLUSH_QUEUE_LIMIT) {
      if (streamFlushRaf !== null) {
        cancelAnimationFrame(streamFlushRaf);
      }
      flushStreamingEvents();
      ensureStreamFlushFallback();
      return;
    }
    if (streamFlushRaf === null) {
      streamFlushRaf = requestAnimationFrame(flushStreamingEvents);
    }
    ensureStreamFlushFallback();
  }

  /** Drop queued events + pending rAF (session/reset paths only). */
  function cancelPendingStreamEvents() {
    if (streamFlushRaf !== null) {
      cancelAnimationFrame(streamFlushRaf);
      streamFlushRaf = null;
    }
    if (streamFlushInterval !== null) {
      clearInterval(streamFlushInterval);
      streamFlushInterval = null;
    }
    pendingStreamEvents = [];
  }

  let processingTimer: ReturnType<typeof setTimeout> | null = null;

  function startProcessingWatchdog() {
    clearProcessingWatchdog();
    processingTimer = setTimeout(() => {
      console.warn("Processing watchdog fired");
      update((s) => ({
        ...s,
        isProcessing: false,
        error: "Processing timed out",
        streamingParts: [],
        messages: s.messages.map((m) => m.streaming ? { ...m, streaming: false, content: m.content || "—interrupted—" } : m),
      }));
    }, PROCESSING_TIMEOUT_MS);
  }

  function clearProcessingWatchdog() {
    if (processingTimer !== null) {
      clearTimeout(processingTimer);
      processingTimer = null;
    }
  }

  function resetProcessingWatchdog() {
    if (processingTimer !== null) {
      clearProcessingWatchdog();
      startProcessingWatchdog();
    }
  }

  function addMessage(msg: Message) {
    update((s) => ({ ...s, messages: [...s.messages, msg], error: null }));
  }

  function generateId(): string {
    return crypto.randomUUID?.() ?? `${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;
  }

  /** Derive the assistant message content from streamingParts (text parts joined). */
  function contentFromParts(parts: UIMessagePart[]): string {
    return parts.filter((p): p is Extract<UIMessagePart, { type: 'text' }> => p.type === 'text')
      .map((p) => p.text)
      .join('');
  }

  /**
   * Attach freshly-built streaming parts to the current state without
   * replacing the messages array (or any historical message object).
   *
   * Streaming deltas arrive at token frequency; replacing the array used
   * to make every ChatMessage's derived inputs dirty on every frame. With
   * the live-message flags now computed once in App.svelte, only the live
   * bubble receives the new `streamingParts` — but the in-place update is
   * still required so callers such as `visibleMessages` can cache by
   * "same array, same last message" instead of re-filtering N messages
   * per token.
   */
  // Incremental join state for the streaming fast path: the common flush
  // only appends to the last text part, so content grows by the delta
  // instead of re-walking every part (O(delta) vs O(total) per frame).
  let streamJoinCache: { partsLen: number; lastPartText: string; content: string } | null =
    null;

  function withStreamingParts(s: ChatState, parts: UIMessagePart[]): ChatState {
    const last = s.messages[s.messages.length - 1];
    if (last && last.role === 'assistant') {
      // Mutate the final assistant message in place; the array identity
      // intentionally stays stable (see T3.1 in the optimization plan).
      const lastPart = parts[parts.length - 1];
      if (
        streamJoinCache &&
        streamJoinCache.partsLen === parts.length &&
        lastPart &&
        lastPart.type === 'text' &&
        typeof lastPart.text === 'string' &&
        lastPart.text.startsWith(streamJoinCache.lastPartText)
      ) {
        last.content =
          streamJoinCache.content +
          lastPart.text.slice(streamJoinCache.lastPartText.length);
        streamJoinCache.lastPartText = lastPart.text;
        streamJoinCache.content = last.content;
      } else {
        last.content = contentFromParts(parts);
        streamJoinCache = {
          partsLen: parts.length,
          lastPartText:
            lastPart && lastPart.type === 'text' && typeof lastPart.text === 'string'
              ? lastPart.text
              : '',
          content: last.content,
        };
      }
    }
    return { ...s, streamingParts: parts };
  }

  return {
    subscribe,

    setModelInfo(info: ModelInfo) {
      update((s) => ({ ...s, modelInfo: info }));
    },

    setSelectedModel(name: string) {
      update((s) => ({ ...s, selectedModel: name }));
    },

    setModelList(list: ModelEntry[]) {
      update((s) => ({ ...s, modelList: list }));
    },

    openModelSwitcher() {
      update((s) => ({ ...s, showModelSwitcher: true }));
    },

    closeModelSwitcher() {
      update((s) => ({ ...s, showModelSwitcher: false }));
    },

    addUserMessage(text: string) {
      addMessage({
        id: generateId(),
        role: "user",
        content: text,
        timestamp: new Date().toISOString(),
        tokensIn: estimateTokens(text),
        children: [],
      });
    },

    /**
     * Render a user interjection as an intervention card INSIDE the
     * current live agent bubble (optimistic, before the backend picks
     * it up). The run is NOT interrupted: no user bubble is appended
     * and the streaming message keeps its identity, so `liveMessageId`
     * stays stable and the footer timer keeps ticking on the one live
     * bubble. Returns the card id, or null when no run is in flight.
     */
    addInterventionCard(text: string): string | null {
      if (!readState().isProcessing) return null;
      // The card must live in a streaming assistant bubble — create one
      // if the run's bubble hasn't been started yet (restored Running
      // session that hasn't received its first protocol event).
      const last = readState().messages[readState().messages.length - 1];
      if (!(last && last.role === "assistant" && last.streaming)) {
        startAssistantMessageInternal();
      }
      const id = `intervention_optimistic_${Date.now()}_${Math.random().toString(36).slice(2, 6)}`;
      update((s) => {
        const parts: UIMessagePart[] = [
          ...s.streamingParts,
          { type: 'data', id, dataType: 'user_intervention', content: text, transient: false },
        ];
        return withStreamingParts(s, parts);
      });
      return id;
    },

    /** Roll back an optimistic intervention card (inject failed). */
    removeInterventionCard(id: string) {
      update((s) => {
        const parts = s.streamingParts.filter(
          (p) => !(p.type === 'data' && p.id === id)
        );
        return withStreamingParts(s, parts);
      });
    },

    startAssistantMessage() {
      startAssistantMessageInternal();
    },

    /**
     * Append text locally (for local-only commands like /help, /sessions, /export).
     * Pushes a new text part rather than merging with the last streaming one.
     */
    appendLocalText(text: string) {
      update((s) => {
        const parts = [...s.streamingParts];
        const id = generatePartId();
        parts.push({ type: 'text', id, text, state: 'streaming' });
        const key = `text:${id}`;
        if (!partArrivalTimes.has(key)) {
          partArrivalTimes.set(key, Date.now());
          partArrivalOrder.push(key);
        }
        return withStreamingParts(s, parts);
      });
    },

    finalizeAssistantMessage(tokensIn?: number, tokensOut?: number, exitReason?: string, preferredContent?: string, contextTokens?: number) {
      clearProcessingWatchdog();
      const currentSid = get(sessions).currentId;
      if (currentSid) sessionCache.delete(currentSid);
      const finalizeStart = Date.now();
      update((s) => {
        const idx = s.messages.length - 1;
        const last = s.messages[idx];
        if (!last || last.role !== "assistant" || !last.streaming) {
          return { ...s, isProcessing: false };
        }

        // Set all streaming parts to 'done' and freeze durations.
        // We use the part-id-keyed `partArrivalTimes` map so the
        // index stays stable even when data_* events push extra
        // parts into `streamingParts` between _start events.
        const finalParts: UIMessagePart[] = s.streamingParts.map((p) => {
          const key = arrivalKey(p);
          const startMs = partArrivalTimes.get(key);

          // For tool-invocation parts, use the tool_output arrival as
          // the end time so the frozen duration reflects tool execution
          // time only (excluding post-tool model thinking).
          let endMs: number | undefined;
          if (p.type === 'tool-invocation') {
            const toolOutputKey = `tool_output:${p.toolCallId ?? ''}`;
            endMs = partArrivalTimes.get(toolOutputKey);
          }

          let nextArr = endMs;
          if (nextArr === undefined && startMs !== undefined) {
            const curIdx = partArrivalOrder.indexOf(key);
            if (curIdx >= 0) {
              for (let j = curIdx + 1; j < partArrivalOrder.length; j++) {
                const nk = partArrivalOrder[j];
                if (nk === key) continue;
                const na = partArrivalTimes.get(nk);
                if (na !== undefined) {
                  nextArr = na;
                  break;
                }
              }
            }
          }
          const dur = Math.max(0, (nextArr ?? finalizeStart) - (startMs ?? finalizeStart));
          if (p.type === 'text') return { ...p, state: 'done' as const, durationMs: dur };
          if (p.type === 'reasoning') return { ...p, state: 'done' as const, durationMs: dur };
          if (p.type === 'tool-invocation') return { ...p, state: 'done' as const, durationMs: p.durationMs ?? dur };
          return { ...p, state: 'done' as const, durationMs: dur };
        }) as UIMessagePart[];

        // Use preferredContent (authoritative full_response from backend) when available;
        // fall back to content assembled from streaming parts.
        const content = preferredContent ?? contentFromParts(finalParts);

        // The final answer text only ever arrives inside `done.full_response`
        // (respond rounds never stream it as text_start/text_delta events).
        // Inject it as a completed text part when no text part already
        // carries it — otherwise a mixed round's partial streamed text
        // masks the `message.content` fallback in ChatMessage.svelte and
        // the final answer never renders live (it only appears after a
        // refresh, when the persisted stream events re-convert the respond
        // call into a text part).
        if (
          preferredContent
          && !finalParts.some((p) => p.type === 'text' && p.text && p.text.trim() === preferredContent.trim())
        ) {
          finalParts.push({ type: 'text', id: generatePartId(), text: preferredContent, state: 'done' as const });
        }

        // Auto-title
        const shouldRename = !get(sessions).sessions.some(
          (ss) => ss.id === get(sessions).currentId && ss.name && !ss.name.startsWith("Session ") && ss.name !== "New Chat"
        );
        if (shouldRename) {
          const firstUser = s.messages.find((m) => m.role === "user");
          if (firstUser && !firstUser.content.startsWith("/")) {
            const title = firstUser.content.replace(/[\n\r]+/g, " ").slice(0, 28).trim();
            if (title) {
              const sid = get(sessions).currentId;
              if (sid) sessions.rename(sid, title + (title.length >= 28 ? "..." : "")).catch(() => {});
            }
          }
        }

        return {
          ...s,
          messages: s.messages.map((m, i) => {
            if (i === idx) {
              return {
                ...m,
                streaming: false,
                content,
                parts: finalParts,
                duration: last.timestamp ? Date.now() - new Date(last.timestamp).getTime() : 0,
                tokensIn: tokensIn ?? m.tokensIn,
                tokensOut: tokensOut ?? (content ? estimateTokens(content) : m.tokensOut),
                contextTokens: contextTokens ?? m.contextTokens,
                exitReason: exitReason ?? m.exitReason,
              };
            }
            // Defensive: any other still-streaming message is an orphan —
            // freeze it so its footer timer stops when the run ends.
            if (m.streaming) {
              const startedMs = m.timestamp ? new Date(m.timestamp).getTime() : NaN;
              return {
                ...m,
                streaming: false,
                duration: m.duration ?? (Number.isFinite(startedMs) ? Math.max(0, Date.now() - startedMs) : 0),
              };
            }
            return m;
          }),
          isProcessing: false,
          streamingParts: [],
          // The task is over — reminders it scheduled die with it (the
          // backend also drops its pending entries and emits
          // reminders_cleared; this covers the local finalize path).
          reminders: [],
        };
      });
      cancelPendingStreamEvents();
      partArrivalTimes.clear();
      partArrivalOrder.length = 0;

      // Refresh the idle view cache with the finalized window so a later
      // switch-back is instant and correct. Reminders stay cleared — they
      // belonged to the finished task, not to the session history.
      if (currentSid) {
        const snap = readState();
        if (!snap.isProcessing) {
          const page = sessionPageState.get(currentSid);
          storeSessionViewCache(currentSid, {
            messages: snap.messages,
            isProcessing: false,
            todos: snap.todos,
            reminders: [],
            pendingAskUser: snap.pendingAskUser,
            modelInfo: snap.modelInfo,
            hasMoreMessages: page?.hasMore ?? snap.hasMoreMessages ?? false,
          });
        }
      }
    },

    setError(err: string) {
      clearProcessingWatchdog();
      cancelPendingStreamEvents();
      const currentSid = get(sessions).currentId;
      if (currentSid) sessionCache.delete(currentSid);
      update((s) => {
        const msgs = [...s.messages];
        // If the last message is an empty assistant bubble (from
        // startAssistantMessage before a failed invoke), remove it.
        const last = msgs[msgs.length - 1];
        if (last && last.role === "assistant" && (!last.content || last.content.trim() === "")) {
          msgs.pop();
        }
        return { ...s, messages: msgs, error: err, isProcessing: false };
      });
    },

    async cancelCurrent() {
      clearProcessingWatchdog();
      const sid = get(sessions).currentId;
      const snap = get(this);
      let tokensIn: number | undefined;
      let tokensOut: number | undefined;
      if (snap.cumulativeInputTokens > 0) {
        tokensIn = snap.cumulativeInputTokens;
        tokensOut = snap.cumulativeOutputTokens;
      } else {
        // Fallback: estimate from all messages in the conversation,
        // preferring stored token counts over content-based estimation.
        try {
          let inSum = 0;
          let outSum = 0;
          for (const m of snap.messages) {
            if (m.role === "user") {
              inSum += m.tokensIn ?? estimateTokens(m.content ?? "");
            } else if (m.role === "assistant") {
              outSum += m.tokensOut ?? estimateTokens(m.content ?? "");
            }
          }
          // If no stored tokens at all, estimate from accumulated message content
          if (inSum === 0 && snap.messages.length > 0) {
            const allText = snap.messages
              .filter(m => m.role === "user")
              .map(m => m.content ?? "")
              .join("\n");
            inSum = estimateTokens(allText);
          }
          const partial = contentFromParts(snap.streamingParts);
          const lastAssistant = snap.messages
            .filter(m => m.role === "assistant")
            .pop();
          const lastContent = lastAssistant?.content ?? "";
          outSum += partial ? estimateTokens(partial) : estimateTokens(lastContent);
          tokensIn = inSum > 0 ? inSum : undefined;
          tokensOut = outSum > 0 ? outSum : undefined;
        } catch (e) {
          console.warn("cancelCurrent token estimation failed:", e);
        }
      }
      this.finalizeAssistantMessage(tokensIn, tokensOut, "stopped_by_user");
      if (sid) {
        try {
          await Promise.race([
            stopSession(sid),
            new Promise((_, reject) =>
              setTimeout(() => reject(new Error("stop timeout")), 3000)
            ),
          ]);
        } catch (e) {
          console.warn("stop session failed or timed out:", e);
        }
      }
    },

    async regenerate() {
      const sid = get(sessions).currentId;
      if (!sid || get(chat).isProcessing) return;
      clearProcessingWatchdog();

      if (isTauri()) {
        update((s) => {
          const msgs = [...s.messages];
          while (msgs.length > 0 && msgs[msgs.length - 1].role === "assistant") {
            msgs.pop();
          }
          return { ...s, messages: msgs };
        });
        this.startAssistantMessage();
        try {
          await tauriInvoke("regenerate", { sessionId: sid });
        } catch (err) {
          this.setError(err instanceof Error ? err.message : String(err));
          this.finalizeAssistantMessage(undefined, undefined, "error");
        }
        return;
      }

      // Web mode: HTTP fetch
      this.startAssistantMessage();
      try {
        const { getAuthToken } = await import("../api/chat");
        const token = getAuthToken();
        const headers: Record<string, string> = { "Content-Type": "application/json" };
        if (token) headers["Authorization"] = `Bearer ${token}`;
        const res = await fetch(`/api/sessions/${sid}/regenerate`, {
          method: "POST",
          headers,
        });
        if (!res.ok) {
          const txt = await res.text().catch(() => "");
          throw new Error(`Regenerate failed: ${res.status} ${txt}`);
        }
      } catch (err) {
        this.setError(err instanceof Error ? err.message : String(err));
        // Finalize the orphaned streaming message so it doesn't hang
        this.finalizeAssistantMessage(undefined, undefined, "error");
      }
    },

    async resume() {
      const sid = get(sessions).currentId;
      if (!sid || get(chat).isProcessing) return;
      clearProcessingWatchdog();

      if (isTauri()) {
        update((s) => {
          const msgs = [...s.messages];
          return { ...s, messages: msgs, isProcessing: false };
        });
        this.startAssistantMessage();
        let modelName: string | undefined;
        update((s) => { modelName = s.selectedModel ?? undefined; return s; });
        try {
          await tauriInvoke("resume_session", { sessionId: sid, modelName });
        } catch (err) {
          this.setError(err instanceof Error ? err.message : String(err));
          this.finalizeAssistantMessage(undefined, undefined, "error");
        }
        return;
      }

      // Web mode
      this.startAssistantMessage();
      try {
        const { getAuthToken } = await import("../api/chat");
        const token = getAuthToken();
        const headers: Record<string, string> = { "Content-Type": "application/json" };
        if (token) headers["Authorization"] = `Bearer ${token}`;
        const res = await fetch(`/api/sessions/${sid}/resume`, {
          method: "POST",
          headers,
          body: JSON.stringify({ message: null }),
        });
        if (!res.ok) {
          const txt = await res.text().catch(() => "");
          throw new Error(`Resume failed: ${res.status} ${txt}`);
        }
      } catch (err) {
        this.setError(err instanceof Error ? err.message : String(err));
        this.finalizeAssistantMessage(undefined, undefined, "error");
      }
    },

    /** Nuclear option: reset all processing state WITHOUT calling the stop API. */
    forceReset() {
      clearProcessingWatchdog();
      cancelPendingStreamEvents();
      const currentSid = get(sessions).currentId;
      if (currentSid) sessionCache.delete(currentSid);
      update((s) => ({
        ...s,
        messages: s.messages.map((m) =>
          m.streaming ? { ...m, streaming: false, content: m.content || "—interrupted—" } : m
        ),
        isProcessing: false,
        error: null,
        streamingParts: [],
        pendingAskUser: null,
      }));
      partArrivalTimes.clear();
      partArrivalOrder.length = 0;
      clearReasoningTimers();
      console.warn("forceReset: frontend state reset (backend agent may still be running)");
    },

    handleSSEEvent(event: SSEEvent) {
      // Reset the 30min watchdog on every event — the agent is still alive.
      resetProcessingWatchdog();
      switch (event.type) {
        case "protocol_v1": {
          const protoEvent = event.data as ProtocolV1Event;
          // ask_user pause signal rides the protocol_v1 envelope;
          // surface it so the dialog opens without a new run.
          if (protoEvent.type === 'ask_user_pending') {
            // The server-side StreamEvent::AskUserPending serialises as
            // `{ type: "ask_user_pending", data: "<json string>" }`,
            // where the JSON string is `{ tool_use_id, tool_name, payload }`
            // and `payload` is the ask_user tool's own output
            // (`{ status, intent, data: { question, candidates } }`).
            const raw = protoEvent as unknown as { data?: string };
            let question = "";
            let candidates: string[] = [];
            let toolUseId: string | undefined;
            if (typeof raw.data === "string" && raw.data) {
              try {
                const outer = JSON.parse(raw.data) as {
                  tool_use_id?: string;
                  payload?: { data?: { question?: string; candidates?: string[] } };
                };
                const inner = outer.payload?.data;
                if (inner && typeof inner.question === "string") question = inner.question;
                if (Array.isArray(inner?.candidates)) candidates = inner!.candidates as string[];
                if (typeof outer.tool_use_id === "string") toolUseId = outer.tool_use_id;
              } catch { /* malformed JSON — fall through with empty values */ }
            }
            this.setPendingAskUser({ question, candidates, askId: toolUseId });
          }
          if (protoEvent.type === 'data_todo_update') {
            const ev = protoEvent as unknown as { items: Array<{id:string;content:string;status:string;priority:string;order:number}>; current: number; total: number };
            if (Array.isArray(ev.items)) {
              this.setTodos(ev.items);
            }
          }
          if (protoEvent.type === 'data_context_usage') {
            const ev = protoEvent as unknown as { current_tokens: number; output_tokens: number; context_window: number; turn: number; message_count: number; total_input_tokens?: number; total_output_tokens?: number };
            update((s) => {
              const msgs = [...s.messages];
              const idx = msgs.length - 1;
              if (idx >= 0 && msgs[idx].role === "assistant") {
                msgs[idx] = {
                  ...msgs[idx],
                  contextTokens: ev.current_tokens,
                  tokensIn: ev.current_tokens,
                  tokensOut: ev.output_tokens > 0 ? ev.output_tokens : msgs[idx].tokensOut,
                };
              }
              return {
                ...s,
                messages: msgs,
                cumulativeInputTokens: ev.total_input_tokens ?? s.cumulativeInputTokens,
                cumulativeOutputTokens: ev.total_output_tokens ?? s.cumulativeOutputTokens,
              };
            });
          }
          if (protoEvent.type === 'data_compressing_context') {
            const ev = protoEvent as unknown as { before_tokens: number; after_tokens: number };
            import("../i18n/index").then(({ localT }) => {
              // after_tokens=0 is the backend's pending marker (P1-h): the
              // summary model is still running. Show a persistent notice —
              // no auto-dismiss timer — until the real numbers arrive.
              if (ev.after_tokens === 0) {
                update((s) => ({
                  ...s,
                  compressionNotice: localT("status.compressingPending", "Compressing context…"),
                }));
                return;
              }
              const notice = localT("status.compressing", "Compressing context: {before} → {after} tokens")
                .replace("{before}", formatTokenCount(ev.before_tokens))
                .replace("{after}", formatTokenCount(ev.after_tokens));
              update((s) => ({ ...s, compressionNotice: notice }));
              setTimeout(() => {
                update((s) => ({ ...s, compressionNotice: null }));
              }, 4000);
            });
          }
          if (protoEvent.type === 'open_artifact') {
            const ev = protoEvent as unknown as {
              artifact_type: string;
              artifact_path: string;
              artifact_label: string;
            };
            if (ev.artifact_type && ev.artifact_path) {
              sidepanel.open({
                type: ev.artifact_type,
                path: ev.artifact_path,
                label: ev.artifact_label,
              }).catch((err) => {
                console.warn("[sidepanel] open_artifact failed:", ev.artifact_path, err);
              });
            }
          }
          if (RENDER_ONLY_EVENTS.has(protoEvent.type)) {
            // Coalesce render-only events (deltas) into one frame.
            queueStreamEvent(protoEvent);
            break;
          }
          // Record arrival time keyed by the part's identity so we
          // can compute per-part durations at finalize time. We
          // use a Map (not an array) because the data_* events push
          // additional parts into streamingParts that don't have
          // their own arrival, which would misalign array indices.
          // Skip non-start events — they update existing parts.
          recordArrivalForEvent(protoEvent);
          resetProcessingWatchdog();
          ensureLiveAssistantMessage();
          update((s) => {
            const parts = [...s.streamingParts];
            applyProtocolEvent(parts, protoEvent);
            const content = contentFromParts(parts);
            const idx = s.messages.length - 1;
            const msgs = s.messages.map((m, i) =>
              i === idx && m.role === "assistant" ? { ...m, content } : m
            );
            return { ...s, streamingParts: parts, messages: msgs };
          });
          break;
        }
        case "done": {
          // Drain any coalesced deltas BEFORE finalizing — otherwise
          // the final message would miss the last frame of tokens.
          if (streamFlushRaf !== null) {
            cancelAnimationFrame(streamFlushRaf);
            flushStreamingEvents();
          }
          const doneEvent = event.data as Record<string, unknown>;
          const doneData = doneEvent?.data as Record<string, unknown> | undefined;
          const exitReason = doneEvent?.exit_reason as string | undefined;
          let responseText: string | undefined;
          let tokensIn: number | undefined;
          let tokensOut: number | undefined;
          let contextTokens: number | undefined;
          if (typeof doneData === "object" && doneData !== null) {
            const dd = doneData as Record<string, unknown>;
            responseText = dd.full_response as string | undefined;
            tokensIn = dd.input_tokens_est as number | undefined;
            tokensOut = dd.output_tokens_est as number | undefined;
            contextTokens = dd.context_tokens_est as number | undefined;
          }
          this.finalizeAssistantMessage(
            tokensIn,
            tokensOut,
            exitReason,
            responseText,
            contextTokens,
          );

          if (tokensIn != null || tokensOut != null) {
            update((s) => {
              const idx = s.messages.length - 1;
              const last = s.messages[idx];
              if (!last || last.role !== "assistant") return s;
              return {
                ...s,
                messages: s.messages.map((m, i) =>
                  i === idx ? {
                    ...m,
                    tokensIn: tokensIn ?? m.tokensIn,
                    tokensOut: tokensOut ?? m.tokensOut,
                    contextTokens: contextTokens ?? m.contextTokens,
                    exitReason: exitReason ?? m.exitReason,
                    content: responseText ?? m.content,
                  } : m
                ),
              };
            });
          }

          // If the agent exited with ASK_USER, the most recent
          // ask_user tool output carries the question and candidate
          // answers. We need to show the AskUserDialog so the user
          // can answer. Without this, the agent just appears to
          // "stop" with no user interaction prompt (the user
          // reported: "ask_user 工具没有弹出窗口让用户选择, 然后就
          // 直接停止任务了").
          if (exitReason === "ASK_USER") {
            const pending = this.findPendingAskUser();
            if (pending) this.setPendingAskUser(pending);
          }
          break;
        }
        case "error":
          this.setError(event.data);
          break;
        case "reminder_fired": {
          // A scheduled/heartbeat reminder fired — decrement its remaining
          // repeats so the right-rail card reflects live status.
          const d = event.data as { message?: string; remaining_repeats?: number };
          if (d && typeof d.message === "string") {
            update((s) => {
              const reminders: ReminderTask[] = s.reminders.map((r) => {
                if (r.title !== d.message) return r;
                const remaining = typeof d.remaining_repeats === "number"
                  ? d.remaining_repeats
                  : Math.max(0, r.remaining - 1);
                return { ...r, remaining, status: remaining > 0 ? "active" : "done" };
              });
              return { ...s, reminders };
            });
          }
          break;
        }
        case "reminders_cleared": {
          // The run ended and the backend dropped this session's pending
          // scheduled/heartbeat reminders — the right-rail cards must go
          // with the task that created them.
          update((s) => (s.reminders.length > 0 ? { ...s, reminders: [] } : s));
          break;
        }
        case "system":
          if (typeof event.data === "string" && event.data === "reminder fired") {
            const sid = get(sessions).currentId;
            // Reloading mid-stream would cancel the pending rAF batch and
            // wipe the live bubble (sessionCache only exists after a
            // switch) — defer until the agent is idle.
            if (sid && !get(this).isProcessing) this.loadSession(sid);
          }
          break;
        case "model_info":
          this.setModelInfo({
            model: event.data.model,
            provider: event.data.provider,
            contextWindow: event.data.context_window,
            isLocal: event.data.is_local,
          });
          break;
        case "ask_user_pending": {
          // The agent loop is blocked waiting for the user — pop the
          // dialog so they can answer. The existing streaming message
          // stays in `isProcessing=true`; we DON'T start a new run.
          const q = event.data?.payload?.data?.question ?? "";
          const cands = Array.isArray(event.data?.payload?.data?.candidates)
            ? (event.data!.payload!.data!.candidates as string[])
            : [];
          this.setPendingAskUser({ question: q, candidates: cands });
          break;
        }
      }
    },

    saveSessionState(sessionId: string) {
      const state = readState();
      if (state.isProcessing && state.messages.length > 0) {
        sessionCache.set(sessionId, {
          messages: state.messages,
          streamingParts: state.streamingParts,
          isProcessing: state.isProcessing,
          pendingAskUser: state.pendingAskUser,
          attachments: state.attachments,
          todos: state.todos,
          compressionNotice: state.compressionNotice,
          modelInfo: state.modelInfo,
        });
      }
      // Detach the live-processing flag from the store: the snapshot above
      // now owns it. Without this reset the flag leaks into the NEXT
      // session selected — applySessionPage saw isProcessing=true and
      // either skipped painting the new page or marked an idle session
      // as Running (the "switch away from a running session breaks the
      // other session" symptom).
      if (state.isProcessing) {
        update((s) => ({ ...s, isProcessing: false, streamingParts: [] }));
        clearProcessingWatchdog();
        cancelPendingStreamEvents();
      }
    },

    async loadSession(sessionId: string) {
      cancelPendingStreamEvents();
      const seq = ++loadSeq;
      // Restore in-progress state from cache if available — prevents
      // the streaming bubble from disappearing when switching sessions.
      const cached = sessionCache.get(sessionId);
      if (cached && cached.isProcessing) {
        set({
          messages: cached.messages,
          isProcessing: cached.isProcessing,
          error: null,
          modelInfo: cached.modelInfo,
          selectedModel: null,
          modelList: [],
          showModelSwitcher: false,
          streamingParts: cached.streamingParts,
          pendingAskUser: cached.pendingAskUser,
          attachments: cached.attachments,
          todos: cached.todos,
          reminders: scanRemindersFromMessages(cached.messages),
          compressionNotice: cached.compressionNotice,
          cumulativeInputTokens: 0,
          cumulativeOutputTokens: 0,
          hasMoreMessages: false,
          loadingSession: false,
        });
        startProcessingWatchdog();
        return;
      }
      sessionCache.delete(sessionId);
      clearProcessingWatchdog();

      // Keep the previous conversation visible while the next page is in
      // flight. The UI renders it under a translucent loading veil.
      update((s) => ({ ...s, loadingSession: true, error: null }));

      // Fast path: a fresh idle-session page in the LRU cache paints
      // synchronously; the server round-trip is skipped entirely.
      const viewCached = sessionViewCache.get(sessionId);
      if (viewCached && Date.now() - viewCached.at < SESSION_VIEW_CACHE_TTL_MS) {
        touchSessionViewCache(sessionId);
        setViewState(sessionId, viewCached.state);
        return;
      }

      try {
        const { getSession } = await import("../api/sessions");
        const data = await getSession(sessionId, {
          offset: 0,
          limit: SESSION_PAGE_SIZE,
        });
        if (seq !== loadSeq) return; // superseded by a newer load
        await applySessionPage(sessionId, data, false, seq);
      } catch (err) {
        if (seq !== loadSeq) return; // superseded by a newer load
        console.error("Failed to load session:", err);
        // Keep the previous list (do not blank the chat); the error banner
        // explains that the new session could not be loaded.
        update((s) => ({
          ...s,
          isProcessing: false,
          error: `Failed to load session: ${err instanceof Error ? err.message : String(err)}`,
          streamingParts: [],
          pendingAskUser: null,
          loadingSession: false,
        }));
      }
    },

    async loadEarlierMessages() {
      const sessionId = get(sessions).currentId;
      if (!sessionId) return;
      const page = sessionPageState.get(sessionId);
      const state = readState();
      if (!page || !page.hasMore || state.isProcessing || state.loadingEarlier) return;
      const offset = page.offset + SESSION_PAGE_SIZE;
      update((s) => ({ ...s, loadingEarlier: true }));
      const seq = loadSeq;
      try {
        const { getSession } = await import("../api/sessions");
        const data = await getSession(sessionId, {
          offset,
          limit: SESSION_PAGE_SIZE,
        });
        await applySessionPage(sessionId, data, true, seq);
      } catch (err) {
        console.warn("Failed to load earlier messages:", err);
        update((s) => (get(sessions).currentId === sessionId
          ? { ...s, loadingEarlier: false }
          : s));
      }
    },

    /** A remote session (not currently visible) finished or errored.
     *  Drop any cached processing snapshot so the next switch fetches
     *  the finalized page from the backend instead of resurrecting the
     *  pre-done streaming state. */
    noteSessionFinished(sessionId: string) {
      sessionCache.delete(sessionId);
      sessionViewCache.delete(sessionId);
      sessionPageState.delete(sessionId);
    },

    clearMessages() {
      clearProcessingWatchdog();
      clearReasoningTimers();
      cancelPendingStreamEvents();
      const currentSid = get(sessions).currentId;
      if (currentSid) {
        sessionCache.delete(currentSid);
        sessionViewCache.delete(currentSid);
        sessionPageState.delete(currentSid);
      }
      update((s) => ({
        messages: [],
        isProcessing: false,
        error: null,
        modelInfo: null,
        streamingParts: [],
        pendingAskUser: null,
        attachments: [],
        selectedModel: s.selectedModel,
        modelList: s.modelList,
        showModelSwitcher: false,
        todos: [],
        reminders: [],
        compressionNotice: null,
        cumulativeInputTokens: 0,
        cumulativeOutputTokens: 0,
      }));
    },

    async submitAskUserResponse(response: string) {
      const text = response.trim();
      if (!text) return;
      const sid = get(sessions).currentId;
      // Reply is delivered to the running agent loop via a dedicated
      // endpoint, NOT as a brand-new user message — the original task
      // resumes in the same run. askId (tool_use_id) lets the loop match
      // this reply to its exact question (P1-i).
      const askId = readState().pendingAskUser?.askId;
      update((s) => ({ ...s, pendingAskUser: null }));
      if (!sid) return;
      try {
        if (isTauri()) {
          await tauriInvoke("ask_user_response", { sessionId: sid, response: text, toolUseId: askId });
        } else {
          const { fetchJson } = await import("../api/chat");
          const res = await fetchJson(`/api/sessions/${sid}/ask_user_response`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ response: text, tool_use_id: askId }),
          });
          if (!res.ok) {
            const detail = await res.text().catch(() => "");
            this.setError(`ask_user response failed: ${res.status} ${detail.slice(0, 200)}`);
          }
        }
      } catch (err) {
        this.setError(err instanceof Error ? err.message : String(err));
      }
    },

    dismissAskUser() {
      // Best-effort: the agent loop is still blocked on ask_user_rx;
      // we just hide the dialog. The loop will unblock once the user
      // submits a real reply, or the run can be cancelled via Stop.
      update((s) => ({ ...s, pendingAskUser: null }));
    },

    /**
     * Walk the most-recent assistant message and find the ask_user
     * tool's payload. The backend stores the ask_user output as a
     * `tool_output_available` event with `name === "ask_user"` and a
     * JSON payload of shape:
     *   {
     *     "status": "INTERRUPT",
     *     "intent": "HUMAN_INTERVENTION",
     *     "data": { "question": "...", "candidates": [...] }
     *   }
     */
    findPendingAskUser(): { question: string; candidates: string[] } | null {
      const snapshot = get(this).messages;
      // Walk the most recent assistant turn.
      for (let i = snapshot.length - 1; i >= 0; i--) {
        const m = snapshot[i];
        if (m.role !== "assistant") continue;
        const parts = m.parts ?? [];
        for (let j = parts.length - 1; j >= 0; j--) {
          const p = parts[j];
          if (p.type !== "tool-invocation") continue;
          if (p.name !== "ask_user") continue;
          const raw = p.result ?? "";
          if (!raw) continue;
          try {
            const parsed = JSON.parse(raw);
            const data = parsed?.data;
            if (parsed?.intent !== "HUMAN_INTERVENTION" && parsed?.status !== "INTERRUPT") {
              continue;
            }
            if (data && typeof data.question === "string") {
              return {
                question: data.question,
                candidates: Array.isArray(data.candidates) ? data.candidates : [],
              };
            }
          } catch {
            // not JSON, ignore
          }
        }
        // Only consider the latest assistant turn.
        break;
      }
      return null;
    },

    setPendingAskUser(pending: PendingAskUser) {
      update((s) => ({ ...s, pendingAskUser: pending }));
    },

    setTodos(items: Array<{id:string;content:string;status:string;priority:string;order:number}>) {
      update((s) => ({ ...s, todos: items }));
    },

    attachFile(file: Attachment) {
      update((s) => ({ ...s, attachments: [...s.attachments, file] }));
    },

    removeAttachment(id: string) {
      update((s) => ({
        ...s,
        attachments: s.attachments.filter((a) => a.id !== id),
      }));
    },

    clearAttachments() {
      update((s) => ({ ...s, attachments: [] }));
    },

    async sendMessage(text: string) {
      // Local commands — handled entirely on the frontend side.
      if (text.trim() === "/resume") {
        this.resume();
        return;
      }

      const sid = get(sessions).currentId;
      if (!sid) return;
      let modelName: string | undefined;
      let attachments: Attachment[] = [];
      update((s) => {
        modelName = s.selectedModel ?? undefined;
        attachments = [...s.attachments];
        s.attachments = [];
        return s;
      });

      // Build message text with attachment markers
      let fullMessage = "";
      for (const a of attachments) {
        if (a.type === "image") {
          fullMessage += `[IMAGE:${a.path}]\n`;
        } else {
          fullMessage += `[FILE:${a.path}]\n`;
        }
      }
      fullMessage += text;

      this.addUserMessage(fullMessage);
      this.startAssistantMessage();
      try {
        await httpSendMessage(fullMessage, sid, undefined, modelName);
      } catch (err) {
        this.setError(err instanceof Error ? err.message : String(err));
      }
    },
  };
}

export const chat = createChatStore();
