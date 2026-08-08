# ADR-0006: Broadcast channel for SSE event bus

## Status

Accepted

## Context

The WebUI server (`openzen serve`) needs to stream real-time agent events (token, thinking,
tool_call, tool_result, done, error, system) to multiple HTTP SSE clients simultaneously.

In the Python version, SSE events were handled per-connection with a simple queue —
each client polling the agent's state. This doesn't scale to multiple concurrent viewers.

Requirements:
- One agent session → N SSE subscribers
- Late joiners should not block early subscribers (non-blocking send)
- Every subscriber sees all events (broadcast, not work-queue)
- Event ordering must be preserved per subscriber

Options: **tokio::sync::broadcast**, **tokio::sync::watch**, **manual subscriber list
with tokio::sync::mpsc**.

## Decision

We use **`tokio::sync::broadcast`** as the SSE event bus, managed by a `SseBus` struct
that wraps the sender and provides a `subscribe()` method returning a receiver.

Key implementation details:
- `broadcast::channel(256)` — capacity of 256 events; slow consumers that lag behind
  get a `Lagged` error and must reconnect
- Events are typed as an enum (`SseEvent<Token, Thinking, ToolCall, ToolResult, Done, Error, System>`)
  serialized to SSE `data:` lines
- The broadcast sender is shared via `Arc` across all HTTP handler clones
- Each SSE connection handler spawns a `tokio::spawn` task that reads from its receiver
  and writes `text/event-stream` responses

## Consequences

**Positive**:
- O(1) publish: broadcasting one event to N subscribers is a single `sender.send()` call
- No manual subscriber management: `broadcast::Receiver` handles per-subscriber state
- Clean integration with axum's SSE response type (`Sse::new(receiver.map(|e| e.to_sse()))`)
- `Lagged` error provides natural backpressure — slow clients disconnect cleanly

**Negative**:
- Fixed channel capacity (256): a flood of events could cause subscriber disconnection
  under heavy load
- `broadcast` requires `Clone` on the event type — every event is cloned per subscriber,
  increasing memory for large tool results
- No per-subscriber filtering: all subscribers receive all event types — clients must
  filter client-side
- Channel capacity must be tuned: too low drops subscribers, too high wastes memory for
  idle connections
