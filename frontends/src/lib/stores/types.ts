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
  streaming?: boolean;
  modelInfo?: ModelInfo;
  /** Why the agent loop finished (e.g. "end_turn", "EXITED", "max_turns", "error") */
  exitReason?: string;
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
  | { type: "protocol_v1"; data: import("./parts").ProtocolV1Event };

export function formatTokenCount(n: number): string {
  if (n >= 1000000) return `${(n / 1000000).toFixed(1)}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}K`;
  return String(n);
}

export function estimateTokens(text: string): number {
  return Math.ceil(text.length / 4);
}
