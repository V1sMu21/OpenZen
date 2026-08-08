---
name: exa-web-search-free
description: Web search via Exa. Use web_search tool for web queries.
metadata: {"clawdbot":{"emoji":"🔍"}}
---

# Web Search

## Core Tools

### web_search
Search web for current info, news, or facts. Returns titles, URLs, and snippets.

### web_fetch
Fetch full page content from a URL. Use after web_search to read detailed content.

## Workflow
1. `web_search(query="...")` → get result URLs
2. `web_fetch(url="...")` → read full page content
