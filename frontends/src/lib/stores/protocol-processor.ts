import type { UIMessagePart, ProtocolV1Event, ToolInvocationPart } from './parts';
import { localT } from '../i18n';

const reasoningStarts = new Map<string, number>();

/** Clean up any dangling reasoning timers (e.g. after reconnect or force reset). */
export function clearReasoningTimers() {
  reasoningStarts.clear();
}

export function applyProtocolEvent(
  parts: UIMessagePart[],
  event: ProtocolV1Event,
): void {
  switch (event.type) {
    case 'reasoning_start':
      reasoningStarts.set(event.id, Date.now());
      if (parts.some((p) => p.type === 'reasoning' && p.id === event.id)) {
        break;
      }
      parts.push({ type: 'reasoning', id: event.id, text: '', state: 'streaming' });
      break;

    case 'reasoning_delta': {
      const p = findLast(parts, (p) => p.type === 'reasoning' && p.id === event.id);
      if (p) (p as Extract<UIMessagePart, { type: 'reasoning' }>).text += event.text;
      break;
    }

    case 'reasoning_end': {
      const p = findLast(parts, (p) => p.type === 'reasoning' && p.id === event.id);
      if (p) {
        (p as Extract<UIMessagePart, { type: 'reasoning' }>).state = 'done';
        const startMs = reasoningStarts.get(event.id);
        if (startMs) {
          (p as Extract<UIMessagePart, { type: 'reasoning' }>).durationMs = Date.now() - startMs;
          reasoningStarts.delete(event.id);
        }
      }
      break;
    }

    case 'text_start':
      if (parts.some((p) => p.type === 'text' && p.id === event.id)) {
        break;
      }
      parts.push({ type: 'text', id: event.id, text: '', state: 'streaming' });
      break;

    case 'text_delta': {
      const p = findLast(parts, (p) => p.type === 'text' && p.id === event.id);
      if (p) (p as Extract<UIMessagePart, { type: 'text' }>).text += event.text;
      break;
    }

    case 'text_end': {
      const p = findLast(parts, (p) => p.type === 'text' && p.id === event.id);
      if (p) (p as Extract<UIMessagePart, { type: 'text' }>).state = 'done';
      break;
    }

    case 'tool_input_start':
      // The agent's synthetic `respond` tool (legacy name `no_tool`)
      // carries the final text reply. The text is already streamed
      // via `text_delta` events during the LLM turn, so rendering a
      // tool card for it would double-display the same content and
      // break the WYSIWYG reading. Skip the part entirely; the
      // `text_delta` stream is the single source of truth. The
      // subsequent `tool_input_delta`/`tool_input_available`/
      // `tool_output_available` events use `findLast` to locate the
      // part by id, so they no-op cleanly when no part exists.
      if (event.name === "respond" || event.name === "no_tool") {
        break;
      }
      // Idempotent: speculative execution can emit a second
      // `tool_input_start` with the same `tool_call_id` after Phase 2
      // already pushed one. Without this guard, Svelte's keyed each
      // would throw `each_key_duplicate` and freeze the chat UI
      // (no further events would render, including the final `done`).
      if (parts.some(
        (p) => p.type === 'tool-invocation' && p.toolCallId === event.tool_call_id
      )) {
        break;
      }
      parts.push({
        type: 'tool-invocation',
        toolCallId: event.tool_call_id,
        name: event.name,
        args: '',
        state: 'input-streaming',
      });
      break;

    case 'tool_input_delta': {
      const p = findLast(
        parts,
        (p) => p.type === 'tool-invocation' && p.toolCallId === event.tool_call_id,
      );
      if (p) (p as ToolInvocationPart).args += event.delta;
      break;
    }

    case 'tool_input_available': {
      const p = findLast(
        parts,
        (p) => p.type === 'tool-invocation' && p.toolCallId === event.tool_call_id,
      );
      if (p) (p as ToolInvocationPart).state = 'input-available';
      break;
    }

    case 'tool_output_available': {
      const p = findLast(
        parts,
        (p) => p.type === 'tool-invocation' && p.toolCallId === event.tool_call_id,
      );
      if (p) {
        const inv = p as ToolInvocationPart;
        inv.state = 'output-available';
        inv.result = event.output;
        // The tool stopped running: drop the heartbeat so the card falls back
        // to the final measured duration instead of a stale "still running".
        inv.liveElapsedSecs = undefined;
      }
      break;
    }

    case 'tool_progress': {
      // Heartbeat for a long-running tool (one per 60s). Attach it to the most
      // recent invocation of the same tool that is still running; when none is
      // running, ignore the event instead of inventing a card for it.
      const p = findLast(
        parts,
        (p) =>
          p.type === 'tool-invocation' &&
          p.name === event.tool_name &&
          p.state !== 'output-available' &&
          p.state !== 'output-error' &&
          p.state !== 'done',
      );
      if (p) {
        (p as ToolInvocationPart).liveElapsedSecs = event.elapsed_secs;
      }
      break;
    }

    case 'data_quality_gate': {
        // Post-answer quality gates (spec synthesis / assertions /
        // independent review) are the silent seconds between the final
        // text and `done` — surface the stage so the pause is legible.
        //
        // One part, updated in place: the three stages arrive seconds apart
        // and each one used to push ANOTHER transient part, so the bar
        // replayed the queue and the array grew on every gate (P2-12).
        const content = localT(`status.qualityGate.${event.stage}`, formatDataEvent(event));
        const existing = parts.find(
          (p) => p.type === 'data' && p.dataType === 'data_quality_gate',
        );
        if (existing) {
          (existing as Extract<UIMessagePart, { type: 'data' }>).content = content;
          // A new stage restarts the notice's lifetime.
          (existing as Extract<UIMessagePart, { type: 'data' }>).id = `data_quality_gate_${Date.now()}`;
          break;
        }
        parts.push({
          type: 'data',
          id: `data_quality_gate_${Date.now()}`,
          dataType: event.type,
          content,
          transient: true,
        });
        break;
      }

    case 'llm_retry': {
        // A wedged gateway (60s header timeout × inner backoff × 8 attempts)
        // used to leave the bubble spinning for 20-40 minutes with no
        // explanation at all. Say what is happening and when the next
        // attempt starts.
        const id = `data_${Date.now()}_${Math.random().toString(36).slice(2, 6)}`;
        parts.push({
          type: 'data',
          id,
          dataType: event.type,
          content: localT('status.llmRetry', formatDataEvent(event))
            .replace('{attempt}', String(event.attempt))
            .replace('{max}', String(event.max_attempts))
            .replace('{secs}', String(event.retry_in_secs)),
          transient: true,
        });
        break;
      }

      // data_compressing_context is handled in chat.ts with i18n + auto-dismiss
      case 'data_todo_update':
      case 'data_context_usage':
      case 'data_compressing_context':
        // Handled in chat.ts store — updates ChatState.
        break;

      case 'user_intervention': {
        // The ChatInput interjection path already pushed an optimistic card
        // into the live bubble (`intervention_optimistic_*` id). Absorb the
        // backend's event into that card instead of appending a duplicate —
        // but only for the FIRST unconfirmed match, so two identical
        // interjections still render two cards.
        const optimistic = parts.find(
          (p) =>
            p.type === 'data' &&
            p.dataType === 'user_intervention' &&
            !p.confirmed &&
            p.id.startsWith('intervention_optimistic_') &&
            p.content === event.content
        );
        if (optimistic) {
          (optimistic as Extract<UIMessagePart, { type: 'data' }>).confirmed = true;
          break;
        }
        parts.push({
          type: 'data',
          id: `intervention_${Date.now()}`,
          dataType: 'user_intervention',
          content: event.content,
          transient: false,
        });
        break;
      }

      // open_artifact is handled in chat.ts (triggers side panel)

    // start_step / finish_step / finish_message are markers
  }
  pruneTransientParts(parts);
}

/** Transient notices are display-only and never belong to the transcript.
 *  `finalizeAssistantMessage` strips them, but a long single turn could still
 *  accumulate thousands before finalize, so keep the tail bounded here too. */
const MAX_TRANSIENT_PARTS = 40;

function pruneTransientParts(parts: UIMessagePart[]): void {
  let count = 0;
  for (const p of parts) if (p.type === 'data' && p.transient) count++;
  if (count <= MAX_TRANSIENT_PARTS) return;
  const drop = count - MAX_TRANSIENT_PARTS;
  let removed = 0;
  for (let i = 0; i < parts.length && removed < drop; i++) {
    const p = parts[i];
    if (p.type === 'data' && p.transient) {
      parts.splice(i, 1);
      i--;
      removed++;
    }
  }
}

function formatDataEvent(event: ProtocolV1Event): string {
  switch (event.type) {
    case 'data_compressing_context':
      return `Compressing context: ${event.before_tokens} → ${event.after_tokens} tokens`;
    case 'data_quality_gate':
      return `Running delivery check: ${event.stage}`;
    case 'llm_retry': {
      const reason =
        event.reason.length > 160 ? `${event.reason.slice(0, 160)}…` : event.reason;
      return `Model not responding — attempt ${event.attempt}/${event.max_attempts} failed, retrying in ${event.retry_in_secs}s (${reason})`;
    }
    default:
      return '';
  }
}

function findLast<T>(arr: T[], predicate: (item: T) => boolean): T | undefined {
  for (let i = arr.length - 1; i >= 0; i--) {
    if (predicate(arr[i])) return arr[i];
  }
  return undefined;
}
