# ADR-0005: CDP + lol_html for HTML simplification

## Status

Accepted

## Context

The `web_scan` tool needs to fetch web pages, simplify HTML content, and present it to
the LLM in a token-efficient format. The Python original used a two-phase approach:
1. Inject JavaScript into the browser via CDP to traverse the DOM
2. Execute `simphtml.js` in-page to produce simplified text

For the Rust rewrite, options considered:
- **Same approach (CDP + JS injection)**: Reuse `simphtml.js` in a headless browser via
  `chromiumoxide` or raw CDP. Requires maintaining the JS dependency.
- **Pure Rust HTML rewriting**: Use `lol_html` to stream-parse HTML and apply CSS selector
  rules, removing non-content elements.
- **Rust HTML parser + tree traversal**: Use `html5ever` + `ego-tree` for full DOM access,
  then filter by rules.

Key trade-off: JS injection requires a full browser engine (CDP connection to Chrome).
Rust-side rewriting can work on raw HTML fetched via HTTP.

## Decision

We use **CDP for DOM acquisition** (via WebSocket to Chrome DevTools Protocol) combined
with **lol_html for Rust-side simplification**.

The process:
1. Connect to Chrome via CDP WebSocket (`Page.enable`, `DOM.getDocument`)
2. Extract raw HTML from the page
3. Use `lol_html::rewrite_str` with content handlers to:
   - Remove `<script>`, `<style>`, `<noscript>`, `<meta>`, `<link>` tags
   - Remove elements matching non-content CSS selectors (`.sidebar`, `nav`, `footer`, etc.)
   - Preserve form values (`input[value]`, `textarea`)
   - Limit DOM depth and output character count

## Consequences

**Positive**:
- CDP handles JavaScript-rendered content (SPA sites) — the tool works on modern web apps
- `lol_html` is a streaming HTML rewriter with zero-copy operations — minimal allocation
- No JS dependency to maintain; simplification rules live in Rust code
- Selector rules can evolve independently of the upstream `simphtml.js`
- CDP abstraction allows future headless-mode support (launch Chrome on demand)

**Negative**:
- Requires a running Chrome instance with `--remote-debugging-port` — adds deployment
  complexity
- CDP protocol versions can drift between Chrome releases; version negotiation needed
- `lol_html` selector engine is less powerful than CSS selectors in a real browser
  (no `:has()`, `:nth-child()` pseudo-classes)
- HTML simplification output may differ from `simphtml.js` output — snapshot tests needed
  for behavioral equivalence
