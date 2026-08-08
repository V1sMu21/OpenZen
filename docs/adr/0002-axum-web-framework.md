# ADR-0002: Axum as web framework

## Status

Accepted

## Context

The project requires an HTTP server for:
- WebUI dashboard (`openzen serve`)
- SSE endpoint for real-time agent events
- MCP (Model Context Protocol) server
- Static file serving for frontend assets

Options considered: **axum**, **actix-web**, **warp**, **hyper** (raw).

Key constraints:
- The entire project is built on tokio; the web framework must integrate cleanly with
  tokio's async runtime
- SSE (Server-Sent Events) is a core requirement — the framework must support streaming
  responses without workarounds
- Static file serving with `ServeDir`-like functionality for the WebUI frontend
- Type-safe extractors for session IDs, query parameters, and shared state

## Decision

We use **axum 0.7** as the web framework.

Rationale:
- Built on tokio + tower, sharing the same async foundation as the rest of the project.
  No separate runtime management needed.
- Extractors (`Path`, `State`, `Query`, `Json`) are composable and type-safe, reducing
  runtime error handling for request parsing.
- SSE is a first-class pattern via `axum::response::sse::Sse` with `tokio_stream::StreamExt`,
  avoiding manual HTTP framing.
- `tower-http::services::ServeDir` provides zero-config static file serving.
- The `Router` nesting model matches the API structure (`/api/chat`, `/api/sessions/:id`,
  `/api/events`, `/`).

## Consequences

**Positive**:
- Single async runtime (tokio) across the entire application — no runtime bridging
- SSE implementation is ~30 lines of Rust vs ~60 lines of Python with manual event framing
- Extractors eliminate an entire class of parsing bugs (wrong query param types, missing keys)
- tower middleware ecosystem (CORS, compression, rate limiting) available for future needs

**Negative**:
- axum 0.7 uses `:id` syntax for path parameters (changed from `{id}` in 0.8+),
  requiring attention during upgrades
- actix-web has a larger production footprint and more community examples for WebSocket
  use cases — we trade this for axum's cleaner tokio integration
- `tower-http` feature flags need explicit management (`cors`, `fs` for ServeDir)
