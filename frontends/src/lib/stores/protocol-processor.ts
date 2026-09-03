import type { UIMessagePart, ProtocolV1Event, ToolInvocationPart } from './parts';

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
      }
      break;
    }

    case 'data_search_stage':
    case 'data_token_meter':
    case 'data_memory_retrieval': {
        const id = `data_${Date.now()}_${Math.random().toString(36).slice(2, 6)}`;
        parts.push({
          type: 'data',
          id,
          dataType: event.type,
          content: formatDataEvent(event),
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
}

function formatDataEvent(event: ProtocolV1Event): string {
  switch (event.type) {
    case 'data_search_stage':
      return `Searching "${event.query}" — found ${event.count} results`;
    case 'data_token_meter':
      return `Tokens: ${event.input} in + ${event.output} out = ${event.total} / ${event.limit}`;
    case 'data_memory_retrieval':
      return `Memory: retrieved ${event.results} results for "${event.query}"`;
    case 'data_compressing_context':
      return `Compressing context: ${event.before_tokens} → ${event.after_tokens} tokens`;
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
