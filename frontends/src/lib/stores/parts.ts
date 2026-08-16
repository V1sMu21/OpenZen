// ── Part Types ──

export type PartState = 'streaming' | 'done';

export type TextPart = {
  type: 'text';
  id: string;
  text: string;
  state: PartState;
  durationMs?: number;
};

export type ReasoningPart = {
  type: 'reasoning';
  id: string;
  text: string;
  state: PartState;
  durationMs?: number;
};

export type ToolInvocationState =
  | 'input-streaming'
  | 'input-available'
  | 'output-available'
  | 'output-error'
  | 'done';

export type ToolInvocationPart = {
  type: 'tool-invocation';
  toolCallId: string;
  name: string;
  args: string;
  state: ToolInvocationState;
  result?: string;
  durationMs?: number;
};

export type UIMessagePart = TextPart | ReasoningPart | ToolInvocationPart | DataPart;

export type DataPart = {
  type: 'data';
  id: string;
  dataType: string;
  content: string;
  transient: boolean;
};

// ── Protocol Event Types ──

export type ProtocolV1Event =
  | { type: 'reasoning_start'; id: string }
  | { type: 'reasoning_delta'; id: string; text: string }
  | { type: 'reasoning_end'; id: string }
  | { type: 'text_start'; id: string }
  | { type: 'text_delta'; id: string; text: string }
  | { type: 'text_end'; id: string }
  | { type: 'tool_input_start'; tool_call_id: string; name: string }
  | { type: 'tool_input_delta'; tool_call_id: string; delta: string }
  | { type: 'tool_input_available'; tool_call_id: string; name: string; args: string }
  | { type: 'tool_output_available'; tool_call_id: string; name: string; output: string }
  | { type: 'start_step' }
  | { type: 'finish_step' }
  | { type: 'finish_message'; stop_reason: string }
  | { type: 'data_search_stage'; stage: string; query: string; count: number }
  | { type: 'data_token_meter'; input: number; output: number; total: number; limit: number }
  | { type: 'data_memory_retrieval'; results: number; query: string }
  | { type: 'data_compressing_context'; before_tokens: number; after_tokens: number }
  | { type: 'data_todo_update'; items: Array<{id:string;content:string;status:string;priority:string;order:number}>; current: number; total: number }
  | { type: 'data_context_usage'; current_tokens: number; output_tokens: number; context_window: number; turn: number; message_count: number; total_input_tokens?: number; total_output_tokens?: number }
  | { type: 'ask_user_pending'; payload?: { data?: { question?: string; candidates?: string[] }; [k: string]: unknown } }
  | { type: 'open_artifact'; artifact_type: string; artifact_path: string; artifact_label: string }
  | { type: 'user_intervention'; content: string };

// ── Helpers ──

let _partIdCounter = 0;
export function generatePartId(): string {
  return `part_${++_partIdCounter}_${Date.now()}`;
}

/** Loose union: accepts both legacy StreamEventItem and the new
 *  protocol_v1 events saved to disk by the backend. Server-side
 *  events have a `duration_ms` field added at save time that we
 *  ignore. */
type SavedEvent = Record<string, unknown> & { type: string };

// Conversion is deterministic for a given stored event array. Historical
// sessions call this repeatedly (loadSession, ChatMessage fallback) and
// legacy event arrays can contain thousands of per-token items, so cache
// the converted result on the array object itself. The array is treated
// as immutable persisted data; if a caller ever mutates one, it must not
// rely on this cache.
const convertedPartsCache = new WeakMap<SavedEvent[], UIMessagePart[]>();

/** Convert stored stream events → UIMessagePart[] for reading from
 *  disk on page reload. Handles both the legacy item format
 *  (`content`/`thinking`/`tool_call`/`tool_result`) and the new
 *  typed protocol_v1 events (`text_*`/`reasoning_*`/`tool_input_*`/
 *  `tool_output_*`). */
import type { StreamEventItem } from './types';

export function convertStreamEventsToParts(items: SavedEvent[]): UIMessagePart[] {
  const cached = convertedPartsCache.get(items);
  if (cached) return cached;

  const parts: UIMessagePart[] = [];
  // Track open text/reasoning/tool-invocation blocks by id so we
  // can append deltas to them. Mirrors what `applyProtocolEvent` does
  // for live streaming.
  const textById = new Map<string, TextPart>();
  const reasoningById = new Map<string, ReasoningPart>();
  const toolById = new Map<string, ToolInvocationPart>();
  // Legacy events used `parts.find(...)` per token; keep O(1) pointers to
  // the first completed text/reasoning part and the FIFO list of tools
  // that still need a result instead.
  let firstDoneText: TextPart | undefined;
  let firstDoneReasoning: ReasoningPart | undefined;
  const unpairedTools: ToolInvocationPart[] = [];

  function noteTextPart(part: TextPart) {
    if (!firstDoneText && part.state === 'done') firstDoneText = part;
  }

  function noteReasoningPart(part: ReasoningPart) {
    if (!firstDoneReasoning && part.state === 'done') firstDoneReasoning = part;
  }

  function noteUnpairedTool(part: ToolInvocationPart) {
    if (part.result == null && !unpairedTools.includes(part)) {
      unpairedTools.push(part);
    }
  }

  function markToolResult(part: ToolInvocationPart, result: string) {
    part.result = result;
    const idx = unpairedTools.indexOf(part);
    if (idx >= 0) unpairedTools.splice(idx, 1);
  }

  function appendToText(id: string, text: string, durationMs?: number) {
    let part = textById.get(id);
    if (!part) {
      part = { type: 'text', id, text: '', state: 'done' };
      textById.set(id, part);
      parts.push(part);
      noteTextPart(part);
    }
    part.text += text;
    if (durationMs !== undefined && durationMs > 0) {
      part.durationMs = durationMs;
    }
  }

  function appendToReasoning(id: string, text: string, durationMs?: number) {
    let part = reasoningById.get(id);
    if (!part) {
      part = { type: 'reasoning', id, text: '', state: 'done' };
      reasoningById.set(id, part);
      parts.push(part);
      noteReasoningPart(part);
    }
    part.text += text;
    if (durationMs !== undefined && durationMs > 0) {
      part.durationMs = durationMs;
    }
  }

  function getOrCreateTool(
    toolCallId: string,
    name?: string,
    args?: string,
  ): ToolInvocationPart {
    let part = toolById.get(toolCallId);
    if (!part) {
      part = {
        type: 'tool-invocation',
        toolCallId,
        name: name ?? '',
        args: args ?? '',
        state: 'input-available',
      };
      toolById.set(toolCallId, part);
      parts.push(part);
      noteUnpairedTool(part);
    } else {
      if (name && !part.name) part.name = name;
      if (args !== undefined) part.args = args;
    }
    return part;
  }

  for (const item of items) {
    switch (item.type) {
      // ── New protocol_v1 events ──
      case 'text_start': {
        const id = String(item.id ?? generatePartId());
        if (!textById.has(id)) {
          const part: TextPart = { type: 'text', id, text: '', state: 'done' };
          textById.set(id, part);
          parts.push(part);
          noteTextPart(part);
        }
        break;
      }
      case 'text_delta': {
        const id = String(item.id ?? '');
        const text = String(item.text ?? '');
        const dur = typeof item.duration_ms === 'number' ? item.duration_ms : undefined;
        if (id) appendToText(id, text, dur);
        break;
      }
      case 'text_end': {
        const id = String(item.id ?? '');
        const p = textById.get(id);
        if (p) {
          p.state = 'done';
          if (typeof item.duration_ms === 'number' && item.duration_ms > 0) {
            p.durationMs = item.duration_ms;
          }
        }
        break;
      }
      case 'reasoning_start': {
        const id = String(item.id ?? generatePartId());
        if (!reasoningById.has(id)) {
          const part: ReasoningPart = { type: 'reasoning', id, text: '', state: 'done' };
          reasoningById.set(id, part);
          parts.push(part);
          noteReasoningPart(part);
        }
        break;
      }
      case 'reasoning_delta': {
        const id = String(item.id ?? '');
        const text = String(item.text ?? '');
        const dur = typeof item.duration_ms === 'number' ? item.duration_ms : undefined;
        if (id) appendToReasoning(id, text, dur);
        break;
      }
      case 'reasoning_end': {
        const id = String(item.id ?? '');
        const p = reasoningById.get(id);
        if (p) {
          p.state = 'done';
          if (typeof item.duration_ms === 'number' && item.duration_ms > 0) {
            p.durationMs = item.duration_ms;
          }
        }
        break;
      }
      case 'tool_input_start': {
        const tcId = String(item.tool_call_id ?? '');
        const name = String(item.name ?? '');
        if (tcId) getOrCreateTool(tcId, name, '');
        break;
      }
      case 'tool_input_delta': {
        const tcId = String(item.tool_call_id ?? '');
        const delta = String(item.delta ?? '');
        if (tcId) {
          const part = getOrCreateTool(tcId);
          part.args += delta;
        }
        break;
      }
      case 'tool_input_available': {
        const tcId = String(item.tool_call_id ?? '');
        const name = String(item.name ?? '');
        const args = String(item.args ?? '');
        if (tcId) {
          const part = getOrCreateTool(tcId, name, args);
          if (part.state === 'input-streaming') part.state = 'input-available';
          // On replay: the backend's gap-based duration from
          // tool_input_available→tool_output_available correctly
          // represents tool execution time.
          if (typeof item.duration_ms === 'number' && item.duration_ms > 0) {
            part.durationMs = item.duration_ms;
          }
        }
        break;
      }
      case 'tool_output_available': {
        const tcId = String(item.tool_call_id ?? '');
        const name = String(item.name ?? '');
        const output = String(item.output ?? '');
        if (tcId) {
          const part = getOrCreateTool(tcId, name);
          part.state = 'output-available';
          markToolResult(part, output);
          // duration_ms is server-attached; if present, use it as
          // an upper bound on the tool's display duration.
          if (typeof item.duration_ms === 'number' && item.duration_ms > 0) {
            part.durationMs = item.duration_ms;
          }
        }
        break;
      }
      case 'finish_message':
      case 'start_step':
      case 'finish_step':
      case 'data_search_stage':
      case 'data_token_meter':
      case 'data_memory_retrieval':
      case 'data_compressing_context':
        // Markers — no-op for replay; we don't re-render progress
        // indicators from disk because they belong to the live
        // session only.
        break;
      // ── Legacy stream-event format ──
      case 'content': {
        // Legacy chat-mode: per-token `content` events must merge
        // into a single text part so the DOM doesn't render
        // thousands of empty <p>'s (this used to freeze the page).
        const txt = String((item as StreamEventItem & { text: string }).text ?? '');
        if (firstDoneText) {
          firstDoneText.text += txt;
        } else {
          const part: TextPart = {
            type: 'text',
            id: generatePartId(),
            text: txt,
            state: 'done',
          };
          textById.set(part.id, part);
          parts.push(part);
          noteTextPart(part);
        }
        break;
      }
      case 'token': {
        // Same as `content` but the older chat-mode delta type.
        const txt = String((item as StreamEventItem & { text: string }).text ?? '');
        if (firstDoneText) {
          firstDoneText.text += txt;
        } else if (txt) {
          const part: TextPart = {
            type: 'text',
            id: generatePartId(),
            text: txt,
            state: 'done',
          };
          textById.set(part.id, part);
          parts.push(part);
          noteTextPart(part);
        }
        break;
      }
      case 'thinking': {
        // Legacy chat-mode: per-token `thinking` events must merge
        // into a single reasoning part. Old sessions stored
        // thousands per message; rendering as separate blocks
        // froze the page for tens of seconds.
        const txt = String((item as StreamEventItem & { text: string }).text ?? '');
        if (firstDoneReasoning) {
          firstDoneReasoning.text += txt;
        } else {
          const part: ReasoningPart = {
            type: 'reasoning',
            id: generatePartId(),
            text: txt,
            state: 'done',
          };
          reasoningById.set(part.id, part);
          parts.push(part);
          noteReasoningPart(part);
        }
        break;
      }
      case 'tool_call': {
        const raw = item as Record<string, unknown>;
        const name = String(raw.name ?? '');
        const args = String(raw.arguments ?? raw.args ?? '');
        const tcId = generatePartId();
        const part: ToolInvocationPart = {
          type: 'tool-invocation',
          toolCallId: tcId,
          name,
          args,
          state: 'output-available',
          durationMs: typeof raw.duration_ms === 'number' ? raw.duration_ms : undefined,
        };
        toolById.set(tcId, part);
        parts.push(part);
        noteUnpairedTool(part);
        break;
      }
      case 'tool_result': {
        // Pair with first unpaired tool_call of same name (FIFO).
        const raw = item as StreamEventItem & { name: string; result: string };
        const target = unpairedTools.find(
          (p2) => p2.name === raw.name && p2.result == null
        );
        if (target) markToolResult(target, raw.result);
        break;
      }
      default:
        // Unknown event — ignore so a single malformed entry doesn't
        // blank the whole message.
        break;
    }
  }

  // The agent loop's synthetic `respond` tool (legacy name `no_tool`)
  // carries the user's final reply in args.response. Render it as
  // plain text rather than a tool card so the chat reads naturally.
  for (let i = 0; i < parts.length; i++) {
    const p = parts[i];
    if (p.type !== 'tool-invocation') continue;
    if (p.name !== 'respond' && p.name !== 'no_tool') continue;
    let replyText = '';
    try {
      const args = JSON.parse(p.args);
      if (typeof args.response === 'string') replyText = args.response;
    } catch { /* not JSON */ }
    if (!replyText && p.result) {
      try {
        const parsed = JSON.parse(p.result);
        if (typeof parsed.response === 'string') replyText = parsed.response;
        else if (typeof parsed === 'string') replyText = parsed;
      } catch { replyText = p.result; }
    }
    if (replyText.trim().length === 0) continue;
    parts[i] = {
      type: 'text',
      id: p.toolCallId,
      text: replyText,
      state: 'done',
      durationMs: p.durationMs,
    } as TextPart;
  }

  convertedPartsCache.set(items, parts);
  return parts;
}
