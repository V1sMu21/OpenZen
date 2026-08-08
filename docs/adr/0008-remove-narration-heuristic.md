# ADR-0008: Remove narration and content heuristic filters

## Status

Accepted

## Context

The original TUI contained two heuristic tag-stripping modules:

- `narration.rs` (375 lines): `NarrationFilter` — stripped `[thinking]`, `[summary]`,
  `[tool_call]`, `[tool_result]` pseudo-tags from the LLM's raw text output.
- `content.rs` (559 lines): `ContentBuffer` — parsed `<thinking>` / `<summary>` XML-style
  tags from the LLM's streaming content and split text into separate typed buffers.

These modules existed because the LLM layer originally emitted undifferentiated text
streams with inline tags. The TUI had to parse the tags on the receiving end.

In Phase 0 (stream protocol migration), the LLM layer was refactored to emit typed
`StreamEvent` events directly:

- `TextStart` / `TextDelta` / `TextEnd` — assistant text
- `ReasoningStart` / `ReasoningDelta` / `ReasoningEnd` — model reasoning
- `ToolInputStart` / `ToolInputDelta` / `ToolInputAvailable` — tool call arguments
- `ToolOutputAvailable` — tool execution results

With these typed events, the TUI no longer needs to strip or parse tags from raw text.
Every event already carries its semantic type, and the TUI's `handle_stream_event`
dispatches each directly to `add_assistant_text()`, `add_thinking_header()`, etc.

The `ProtocolAdapter` (which previously replicated events in tagged string format)
was also deleted. The SSE bus now sends only `protocol_v1` JSON events.

## Decision

Delete `narration.rs`, `content.rs`, and `protocol_adapter.rs` entirely.

All tag-stripping and heuristic parsing logic is removed from the codebase. The TUI,
WebUI, and Tauri desktop now consume only typed `protocol_v1` events from the LLM layer.

## Consequences

### Positive
- Removed 1,100+ lines of heuristic parsing code (complex, error-prone regex/tag matching).
- Eliminated double-send of events (old tagged format + new typed format).
- Simplified the TUI event loop by removing `NarrationFilter::feed()` and
  `ContentBuffer` state management.
- Single source of truth: LLM layer produces typed events, UIs consume them directly.

### Negative
- Loss of backward compatibility with old session JSON files that contain tagged raw text.
  Old sessions are still readable via `legacy_session_reader.rs` for message content,
  but reasoning/tool metadata from pre-Phase-0 sessions is not reconstructed.
- No heuristic fallback if a new LLM provider emits tags in text content. This is
  considered acceptable since all current providers (Claude, OpenAI, MiniMax) have
  structured content block APIs.

## Alternatives Considered

1. **Keep both paths**: Maintain the heuristic modules as a fallback while also consuming
   typed events. Rejected — doubles maintenance burden and creates confusion about which
   path is authoritative.
2. **Keep only content.rs**: Strip `<thinking>` tags from text content but not the
   full narration filter. Rejected — partial heuristic is worse than none; better
   to fix the source (LLM layer) to produce typed events.
3. **ProtocolAdapter as legacy bridge**: Keep adapter for old session compatibility only.
   Rejected — old sessions are a shrinking concern; the adapter added 250+ lines of
   mapping logic for edge cases that diminish over time.
