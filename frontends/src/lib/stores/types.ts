export interface ToolCallInfo {
  name: string;
  arguments: string;
  result?: unknown;
}

export interface ModelInfo {
  model: string;
  provider: string;
  contextWindow: number;
  isLocal: boolean;
}

/** @legacy Used only for reading old session history. New sessions use ProtocolV1Event. */
export type StreamEventItem =
  | { type: "content"; text: string; durationMs?: number }
  | { type: "thinking"; text: string; durationMs?: number }
  | { type: "tool_call"; name: string; arguments: string; durationMs?: number }
  | { type: "tool_result"; name: string; result: string; durationMs?: number };

export interface Message {
  id: string;
  role: "user" | "assistant" | "system" | "tool";
  content: string;
  thinking?: string;
  toolCalls?: ToolCallInfo[];
  /** Ordered list of completed events (saved when message finalizes) */
  streamEvents?: StreamEventItem[];
  /** Protocol parts (saved when message finalizes, replaces streamEvents for new messages) */
  parts?: import("./parts").UIMessagePart[];
  timestamp: string;
  tokensIn?: number;
  tokensOut?: number;
  contextTokens?: number;
  contextUsed?: number;
  contextWindow?: number;
  duration?: number;
  /**
   * Wall-clock time this turn FINISHED.
   *
   * `timestamp` is the turn's START for live turns (the live footer timer
   * counts `now - timestamp`), but the backend persists the assistant message
   * only at after_run — so a saved message's `timestamp` IS its finish time.
   * Deriving the footer time as `timestamp + duration` therefore pushed
   * restored turns up to a whole task-length into the future (a 34-minute run
   * displayed a time 34 minutes late). Set explicitly at finalize; messages
   * loaded from disk carry their persisted timestamp here.
   */
  completedAt?: string;
  streaming?: boolean;
  modelInfo?: ModelInfo;
  /** Why the agent loop finished (e.g. "end_turn", "EXITED", "max_turns", "error") */
  exitReason?: string;
  /** Delivery-quality outcome reported by the backend (R5-5). Absent on
   *  messages persisted before this existed — treated as "not verified". */
  quality?: MessageQuality;
  /** Position in the session's raw message list, as reported by
   *  `get_session`. Needed to address one message in feedback commands. */
  serverIndex?: number;
  /** What the user said about this answer (P2-28). Local echo only. */
  feedback?: "up" | "down";
  parentId?: string;
  children: string[];
}

export type SSEEvent =
  | { type: "token"; data: string }
  | { type: "thinking"; data: string }
  | { type: "tool_call"; data: { name: string; arguments: string } }
  | { type: "tool_result"; data: { name: string; result: string } }
  | { type: "done"; data: { exit_reason: string; data?: string; tokens_in?: number; tokens_out?: number } }
  | { type: "error"; data: string }
  | { type: "system"; data: string }
  | { type: "model_info"; data: { model: string; provider: string; context_window: number; is_local: boolean } }
  /** ask_user fired; the loop is blocked on the user's reply (NOT a new run). */
  | { type: "ask_user_pending"; data: { tool_use_id: string; tool_name: string; payload: { data?: { question?: string; candidates?: string[] }; [k: string]: unknown } } }
  /** Approval required before the next tool call can execute. */
  | { type: "approval_needed"; data: { request_id: string; session_id?: string; tool_name: string; pattern: string; arguments?: unknown; approved_count?: number; current_level?: string } }
  /** A scheduled/heartbeat reminder fired — decrements the card's repeats. */
  | { type: "reminder_fired"; data: { message: string; remaining_repeats?: number } }
  /** The run ended — its pending scheduled/heartbeat reminders were dropped
   *  backend-side; the right-rail cards must clear too. */
  | { type: "reminders_cleared"; data: null }
  /** Protocol v1 event — data is a ProtocolV1Event JSON object */
  | { type: "protocol_v1"; data: import("./parts").ProtocolV1Event }
  /** Backend-side coalescing: one SSE message carrying an ordered burst of
   *  protocol events. Expanded by sse.ts into individual protocol_v1 events. */
  | { type: "protocol_v1_batch"; data: { events: import("./parts").ProtocolV1Event[] } };

export function formatTokenCount(n: number): string {
  if (n >= 1000000) return `${(n / 1000000).toFixed(1)}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}K`;
  return String(n);
}

/** Relative time for a backend timestamp (RFC3339, epoch seconds, epoch ms,
 *  or epoch nanos — the memory store uses nanos). Returns "—" for anything
 *  unparseable so a bad value never renders as "Invalid Date". */
export function formatWhen(value: string | number | null | undefined): string {
  if (value === null || value === undefined || value === "") return "—";
  let ms: number;
  if (typeof value === "number") {
    ms = value > 1e15 ? value / 1e6 : value > 1e11 ? value : value * 1000;
  } else if (/^\d+$/.test(value)) {
    const n = Number(value);
    ms = n > 1e15 ? n / 1e6 : n > 1e11 ? n : n * 1000;
  } else {
    ms = Date.parse(value);
  }
  if (!Number.isFinite(ms)) return "—";
  const diff = Date.now() - ms;
  if (diff < 0) return new Date(ms).toLocaleString();
  const min = Math.floor(diff / 60000);
  if (min < 1) return "just now";
  if (min < 60) return `${min}m`;
  const h = Math.floor(min / 60);
  if (h < 24) return `${h}h`;
  const d = Math.floor(h / 24);
  if (d < 30) return `${d}d`;
  return new Date(ms).toLocaleDateString();
}

/** Delivery-quality state of one assistant turn.
 *
 *  The backend reports it as `message.quality.review_status` (+ optional
 *  `assertion_status`). Three states must be distinguishable in the UI —
 *  "checked and passed", "checked, problems found, released anyway" and
 *  "never checked" are very different promises, and collapsing them into
 *  "no badge" made an unverified delivery look verified (R5-5). */
export type ReviewStatus =
  | "passed"
  | "issues_but_released"
  | "unavailable"
  | "not_run";

export interface MessageQuality {
  review_status?: ReviewStatus | string;
  assertion_status?: string;
  /** Free-text note from the gate (e.g. which assertion failed). */
  note?: string;
}

export interface QualityContractView {
  /** Normalized status; unknown/missing input becomes "not_run". */
  status: ReviewStatus;
  /** True when the badge must be visible without hovering. */
  visible: boolean;
  tone: "ok" | "warn" | "muted";
  labelKey: string;
}

/** Normalize whatever the backend sent into the three-state view. Anything
 *  unrecognized is treated as "not_run" — never as a pass. */
export function qualityContract(
  q: MessageQuality | null | undefined,
): QualityContractView {
  const raw = typeof q?.review_status === "string" ? q.review_status : "";
  switch (raw) {
    case "passed":
      return { status: "passed", visible: true, tone: "ok", labelKey: "quality.passed" };
    case "issues_but_released":
      return {
        status: "issues_but_released",
        visible: true,
        tone: "warn",
        labelKey: "quality.issuesButReleased",
      };
    case "unavailable":
      return {
        status: "unavailable",
        visible: true,
        tone: "warn",
        labelKey: "quality.unavailable",
      };
    case "not_run":
      return { status: "not_run", visible: true, tone: "muted", labelKey: "quality.notRun" };
    default:
      // No data at all: say "not verified" rather than showing nothing.
      return { status: "not_run", visible: true, tone: "muted", labelKey: "quality.notRun" };
  }
}

export function estimateTokens(text: string): number {
  return Math.ceil(text.length / 4);
}
