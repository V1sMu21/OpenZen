# Stream Protocol Migration Plan

## Goal
Replace OpenZen's heuristic-based streaming pipeline (3-layer regex tag stripping, fallback type guessing, post-hoc tool call matching) with a typed "start-delta-end" protocol architecture modeled after Vercel AI SDK v5's `UIMessage.parts`, implemented entirely in pure Rust + TypeScript + Svelte 5 — no React, no AI SDK dependency.

## Success Criteria
1. All 5 heuristic functions (`stripLiveSummary`, `bufferPartialTag`, `extractThinkingBlocks`, `stripFunctionCallArtifacts`, `stripThinkingTags`) removed
2. `fallbackThinking` and `fallbackToolCalls` logic removed from ChatMessage.svelte
3. `displayItems` merge logic replaced by direct `parts` array iteration
4. `streamingContent + completedItems + inSummary + summaryHold` replaced by single `parts[]` on streaming message
5. Tag incompleteness failures eliminated: any model output is correctly classified as text/reasoning/tool with zero guesswork
6. LSP diagnostics clean on all changed files
7. Existing card components (ThinkingBlock, ToolCallCard, EditCard, StreamingText) remain functional — only their data source changes

---

## Phase 1 — Backend: Protocol Events (Rust)

### 1.1 Extend StreamEvent enum

**File:** `crates/ga-core-types/src/event.rs`

Current enum has flat `Token`, `Thinking`, `ToolCall`, `ToolCallReady`, `ToolResult`, `Done`, `Error`.

Add new protocol-level variants alongside existing ones (backward-compatible during transition):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    // ── Existing (keep during transition, remove in cleanup) ──
    Token { text: String },
    Thinking { text: String },
    ToolCall { name: String, args: String },
    ToolCallReady { id: String, name: String, args: String },
    ToolResult { name: String, result: String },
    Done { exit_reason: String, full_response: String, full_thinking: String, data: String },
    Error { message: String },

    // ── NEW: Protocol events ──
    /// A reasoning block has started.
    ReasoningStart { id: String },
    /// Incremental reasoning content.
    ReasoningDelta { id: String, text: String },
    /// The reasoning block is complete.
    ReasoningEnd { id: String },

    /// A text block has started.
    TextStart { id: String },
    /// Incremental text content.
    TextDelta { id: String, text: String },
    /// The text block is complete.
    TextEnd { id: String },

    /// Tool input streaming has started.
    ToolInputStart { tool_call_id: String, name: String },
    /// Incremental tool input (JSON partial).
    ToolInputDelta { tool_call_id: String, delta: String },
    /// Tool input is complete and ready for execution.
    ToolInputAvailable { tool_call_id: String, name: String, args: String },
    /// Tool has executed and output is available.
    ToolOutputAvailable { tool_call_id: String, name: String, output: String },

    /// Protocol-level markers
    StartStep {},
    FinishStep {},
    FinishMessage { stop_reason: String },
}
```

**Verification:**
- [ ] `cargo build` passes
- [ ] Serialization round-trip test for each new variant

### 1.2 Create Model Adapter

**New file:** `crates/ga-server/src/webui/protocol_adapter.rs`

Single responsibility: **transform raw LLM stream events into protocol events**. This is the single authority for content classification.

```rust
pub struct ProtocolAdapter {
    event_tx: mpsc::UnboundedSender<StreamEvent>,
    next_reasoning_id: AtomicU64,
    next_text_id: AtomicU64,
}
```

**For Claude (Anthropic API):**
- `thinking_delta` → `ReasoningStart` (first) + `ReasoningDelta`*
- `text_delta` → `TextStart` (first) + `TextDelta`*
- `content_block_start` with type `tool_use` → `ToolInputStart`
- `input_json_delta` → `ToolInputDelta`*
- `content_block_stop` with tool_use → `ToolInputAvailable`
- Tool execution result → `ToolOutputAvailable`

**For OpenAI-compatible (Minimax, etc.):**
- `reasoning_content` field → `ReasoningStart` + `ReasoningDelta`*
- `content` field → `TextStart` + `TextDelta`*
- `tool_calls` in delta → `ToolInputStart` + `ToolInputDelta`* + `ToolInputAvailable`

**For problematic providers (Minimax with `reasoning_split=false`):**
- Parse `<thinking>...</thinking>` from content ONCE in the adapter
- Emit correct protocol events — frontend never sees raw tags

**Integration point:** In `sse_bus.rs`, route events through `ProtocolAdapter` before broadcasting to SSE clients. Old `Token`/`Thinking`/`ToolCall` events continue to be emitted alongside new ones during transition.

**Verification:**
- [ ] Unit tests for each provider type (Claude, OpenAI reasoning_content, Minimax thinking-tags-in-content)
- [ ] Tag boundary test: partial `<thinking` in one chunk, `>` in next → correctly classified

### 1.3 Update SSE Bus

**File:** `crates/ga-server/src/webui/sse_bus.rs`

New protocol events are serialized as SSE and prefixed with a protocol version marker:

```
event: protocol_v1
data: {"type":"reasoning_start","id":"r1"}

event: protocol_v1
data: {"type":"reasoning_delta","id":"r1","text":"thinking..."}

event: protocol_v1
data: {"type":"reasoning_end","id":"r1"}
```

Old-format events (`token`, `thinking`, `tool_call`, `tool_result`) continue to be sent alongside new ones during transition so the old frontend code path still works.

**Verification:**
- [ ] SSE stream contains both old and new events
- [ ] Existing frontend code path unaffected
- [ ] New protocol events parseable by frontend

---

## Phase 2 — Frontend: Type System (TypeScript)

### 2.1 Define UIMessagePart types

**New file:** `frontends/src/lib/stores/parts.ts`

Replace `StreamEventItem` with a proper discriminated union:

```typescript
// === Part Types ===

export type PartState = 'streaming' | 'done';

export type TextPart = {
  type: 'text';
  id: string;
  text: string;
  state: PartState;
};

export type ReasoningPart = {
  type: 'reasoning';
  id: string;
  text: string;
  state: PartState;
};

export type ToolInvocationState =
  | 'input-streaming'
  | 'input-available'
  | 'output-available'
  | 'output-error';

export type ToolInvocationPart = {
  type: 'tool-invocation';
  toolCallId: string;
  name: string;
  args: string;
  state: ToolInvocationState;
  result?: string;
  durationMs?: number;
};

export type UIMessagePart = TextPart | ReasoningPart | ToolInvocationPart;

// === Protocol Event Types ===

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
  | { type: 'finish_message'; stop_reason: string };
```

### 2.2 Protocol Event Processor

**New file:** `frontends/src/lib/stores/protocol-processor.ts`

Pure function that mutates a parts array in response to each protocol event:

```typescript
import type { UIMessagePart, ProtocolV1Event } from './parts';

export function applyProtocolEvent(
  parts: UIMessagePart[],
  event: ProtocolV1Event,
): void {
  switch (event.type) {
    case 'reasoning_start':
      parts.push({ type: 'reasoning', id: event.id, text: '', state: 'streaming' });
      break;

    case 'reasoning_delta': {
      const p = parts.findLast(p => p.type === 'reasoning' && p.id === event.id);
      if (p) p.text += event.text;
      break;
    }

    case 'reasoning_end': {
      const p = parts.findLast(p => p.type === 'reasoning' && p.id === event.id);
      if (p) p.state = 'done';
      break;
    }

    case 'text_start':
      parts.push({ type: 'text', id: event.id, text: '', state: 'streaming' });
      break;

    case 'text_delta': {
      const p = parts.findLast(p => p.type === 'text' && p.id === event.id);
      if (p) p.text += event.text;
      break;
    }

    case 'text_end': {
      const p = parts.findLast(p => p.type === 'text' && p.id === event.id);
      if (p) p.state = 'done';
      break;
    }

    case 'tool_input_start':
      parts.push({
        type: 'tool-invocation',
        toolCallId: event.tool_call_id,
        name: event.name,
        args: '',
        state: 'input-streaming',
      });
      break;

    case 'tool_input_delta': {
      const p = parts.findLast(
        p => p.type === 'tool-invocation' && p.toolCallId === event.tool_call_id,
      );
      if (p) p.args += event.delta;
      break;
    }

    case 'tool_input_available': {
      const p = parts.findLast(
        p => p.type === 'tool-invocation' && p.toolCallId === event.tool_call_id,
      );
      if (p) p.state = 'input-available';
      break;
    }

    case 'tool_output_available': {
      const p = parts.findLast(
        p => p.type === 'tool-invocation' && p.toolCallId === event.tool_call_id,
      );
      if (p) {
        p.state = 'output-available';
        p.result = event.output;
      }
      break;
    }

    // start_step / finish_step / finish_message are markers for multi-step
  }
}
```

**Key property:** This is the ONLY place where parts mutation logic lives. No scattered appendToken/appendThinking/setToolCall functions.

**Verification:**
- [ ] Unit tests: reasoning_start → delta → end produces single part with merged text
- [ ] Unit tests: interleaved reasoning and text events produce correctly ordered parts
- [ ] Unit tests: tool_input_start → delta → available → output produces correct state transitions

---

## Phase 3 — Frontend: Store Rewrite

### 3.1 Update ChatState

**File:** `frontends/src/lib/stores/chat.ts`

Replace:
```typescript
streamingContent: string;
completedItems: StreamEventItem[];
inSummary: boolean;
summaryHold: string;
```

With:
```typescript
/// The parts of the currently streaming assistant message.
/// During streaming, the last part may have state='streaming'.
streamingParts: UIMessagePart[];
```

Message type adds:
```typescript
interface Message {
  // ...existing fields
  parts?: UIMessagePart[];  // persisted, replaces streamEvents
}
```

### 3.2 Rewrite event handlers

Replace these functions entirely:

| Old | New |
|---|---|
| `appendToken` | Removed — replaced by protocol event processing |
| `appendThinking` | Removed |
| `setToolCall` | Removed |
| `setToolResult` | Removed — replaced by `tool_output_available` handler |
| `finalizeAssistantMessage` | Simplified: just copies `streamingParts` → `message.parts` |

New `handleSSEEvent` becomes:

```typescript
handleSSEEvent(event: SSEEvent) {
  switch (event.type) {
    case 'protocol_v1': {
      const protoEvent = event.data as ProtocolV1Event;
      update((s) => {
        const parts = [...s.streamingParts];
        applyProtocolEvent(parts, protoEvent);
        // Also update message.content for backward compat
        const text = parts.filter(p => p.type === 'text').map(p => p.text).join('');
        return {
          ...s,
          streamingParts: parts,
          messages: updateLastMessage(s.messages, { content: text }),
        };
      });
      break;
    }
    // Old event types still handled for transition
    case 'token': this.appendToken(event.data); break;
    // ... etc
  }
}
```

### 3.3 Message persistence

Update `loadSession` to handle both old (`streamEvents`) and new (`parts`) formats:

```typescript
const parts: UIMessagePart[] = m.parts ?? convertStreamEventsToParts(m.streamEvents);
```

Write a one-way converter `convertStreamEventsToParts` that transforms old `StreamEventItem[]` to `UIMessagePart[]` — this is for reading old sessions only, not for live streaming.

### 3.4 Remove all heuristic functions

Delete these functions (check for callers first):

| Function | File | Lines |
|---|---|---|
| `stripLiveSummary` | chat.ts | 126-154 |
| `bufferPartialTag` | chat.ts | 162-180 |
| `extractThinkingBlocks` | chat.ts | 182-194 |
| `stripFunctionCallArtifacts` | chat.ts | 196-202 |
| `reconstructStreamEvents` | (some store file) | — |

**Verification:**
- [ ] grep for each function name returns 0 results (except in git history)
- [ ] `npm run build` passes
- [ ] `lsp_diagnostics` clean

---

## Phase 4 — Frontend: Component Rewrite

### 4.1 Update ChatMessage.svelte

**File:** `frontends/src/lib/components/ChatMessage.svelte`

Replace:

```svelte
<!-- OLD: heuristic-based display items -->
{#each displayItems as evt}
  {#if evt.type === "thinking"}
    <ThinkingBlock ... />
  {:else if evt.type === "tool_call"}
    <ToolCallCard ... />
  {:else if evt.type === "content"}
    {@html renderMarkdown(stripThinkingTags(evt.text))}
  {/if}
{/each}
{#if fallbackThinking} ... {/if}
{#if fallbackToolCalls} ... {/if}
```

With:

```svelte
<!-- NEW: type-safe parts iteration -->
{#each parts as part (part.id)}
  {#if part.type === "reasoning"}
    <ThinkingBlock
      thinking={part.text}
      streaming={part.state === "streaming"}
    />
  {:else if part.type === "tool-invocation"}
    <ToolCallCard
      toolCall={{ name: part.name, arguments: part.args, result: part.result }}
      collapsed={true}
    />
  {:else if part.type === "text" && part.text}
    <div class="content-block">{@html renderMarkdown(part.text)}</div>
  {/if}
{/each}
```

**Key deletions from ChatMessage.svelte:**
- `displayItems` derivation (line 44-98)
- `fallbackThinking` derivation (line 100-102)
- `fallbackToolCalls` derivation (line 103-107)
- `stripThinkingTags` function (line 156-161)
- `toolStats` aggregation (line 212-224) — keep if still useful but derive from `parts`
- Any content merging in the template

**Verification:**
- [ ] All card components (ThinkingBlock, ToolCallCard, EditCard, StreamingText) still render
- [ ] `streaming=true` prop works correctly for reasoning parts in streaming state
- [ ] `durationMs` correctly assigned from timing logic
- [ ] Copy content function uses `parts` instead of `stripThinkingTags(message.content)`

### 4.2 StreamingText component

**File:** `frontends/src/lib/components/StreamingText.svelte`

Currently takes `text` prop and renders it via `renderMarkdown()`. This stays the same — but now its input comes from `parts` where `type === 'text'`, instead of from `liveContent` (which was derived from `streamingContent`).

### 4.3 TypeScript type cleanup

**Remove unused types:**
- `StreamEventItem` type (or reduce to legacy converter only)
- `ToolCallInfo` if replaced by ToolInvocationPart

---

## Phase 5 — Backend: Cleanup

### 5.1 Remove old event emission

Once frontend only uses protocol events:
- Remove `StreamEvent::Token`, `Thinking`, `ToolCall`, `ToolResult` emissions from backend
- Keep `StreamEvent::Done` for finalization (or replace with `FinishMessage`)

### 5.2 Remove old variants from enum

Clean up `event.rs` to remove `Token`, `Thinking`, `ToolCall`, `ToolCallReady` variants.

---

## Files Changed Summary

### Rust (backend)
| File | Change |
|---|---|
| `crates/ga-core-types/src/event.rs` | Add 14 new protocol variants, keep old 7 for transition |
| `crates/ga-server/src/webui/protocol_adapter.rs` | **NEW** ~200 lines — transform raw events to protocol events |
| `crates/ga-server/src/webui/sse_bus.rs` | Route events through ProtocolAdapter, add protocol_v1 SSE format |
| `crates/ga-server/Cargo.toml` | (possibly) no new deps needed |

### TypeScript/Svelte (frontend)
| File | Change |
|---|---|
| `frontends/src/lib/stores/parts.ts` | **NEW** ~100 lines — TypeScript types for parts + protocol events |
| `frontends/src/lib/stores/protocol-processor.ts` | **NEW** ~120 lines — applyProtocolEvent pure function |
| `frontends/src/lib/stores/chat.ts` | **REWRITE** ~400 lines — replace streamingContent/completedItems with streamingParts, remove 5 heuristic functions |
| `frontends/src/lib/stores/sse.ts` | ~10 lines — handle `protocol_v1` event type |
| `frontends/src/lib/components/ChatMessage.svelte` | **REWRITE** ~200 lines — replace displayItems with parts iteration, remove fallback logic |
| `frontends/src/lib/types.ts` | (possibly) remove StreamEventItem, add UIMessagePart types |

### Estimated total: ~400 lines new, ~800 lines deleted, ~600 lines modified

---

## Migration Order (dependency chain)

```
Phase 1 (backend events)
    ↓
Phase 2.1 (frontend types) ──→ Phase 2.2 (protocol processor)
                                        ↓
                              Phase 3 (store rewrite)
                                        ↓
                              Phase 4 (component rewrite)
                                        ↓
                              Phase 5 (backend cleanup)

Phase 1 and Phase 2.1 can run in parallel.
Phase 2.2, 3, 4 must run sequentially.
```

---

## Risk Mitigation

| Risk | Mitigation |
|---|---|
| Old sessions won't render with new code | Write `convertStreamEventsToParts` to transform persisted data — old sessions work immediately |
| Backward compat during deploy | Emit both old and new event formats during transition. Old frontend ignores new events. New frontend uses new events, falls back to old processing for backward compat |
| Provider-specific quirk not covered by adapter | Unit tests per provider. Add a `reasoning_tag` config option per model that tells the adapter how to extract reasoning (e.g. `"<think>"`, `"<reasoning>"`, `"none"` for native `reasoning_content`) |
| TypeScript build errors from deleted types | Keep types during transition, remove in cleanup phase after verifying no references |
| Svelte 5 reactivity issues with parts array | Use `$state()` and array mutation inside `$effect()` — Svelte 5 tracks array `.push()`/.splice() if using `$state` or `$derived`. Or use immutable `[...parts]` pattern |
