import katex from "katex";

/**
 * ── Semantic keyword highlighting ─────────────────────────────
 * Keywords in agent replies are wrapped in colored spans so users
 * can scan for important terms at a glance:
 *   .kw-green  → success / positive results
 *   .kw-orange → warnings / attention
 *   .kw-red    → errors / failures
 * Rules are matched longest-first to avoid partial overlaps
 * ("测试通过" wins over "通过"). Chinese terms use a negation
 * lookbehind so "没有问题 / 无错误" are NOT flagged as warnings.
 * Edit this table to tune the vocabulary.
 */
const KEYWORD_HIGHLIGHTS: ReadonlyArray<readonly [word: string, cls: string]> = [
  // ── Green — success / positive ──
  ["全部通过", "kw-green"],
  ["编译通过", "kw-green"],
  ["测试通过", "kw-green"],
  ["运行成功", "kw-green"],
  ["已成功", "kw-green"],
  ["已解决", "kw-green"],
  ["已修复", "kw-green"],
  ["已恢复", "kw-green"],
  ["已安装", "kw-green"],
  ["已更新", "kw-green"],
  ["已启动", "kw-green"],
  ["已连接", "kw-green"],
  ["无错误", "kw-green"],
  ["没有错误", "kw-green"],
  ["成功", "kw-green"],
  ["通过", "kw-green"],
  ["修复", "kw-green"],
  ["解决", "kw-green"],
  ["完成", "kw-green"],
  ["正常", "kw-green"],
  ["可用", "kw-green"],
  ["正确", "kw-green"],
  ["恢复", "kw-green"],
  ["passed", "kw-green"],
  ["success", "kw-green"],
  ["pass", "kw-green"],
  ["ok", "kw-green"],

  // ── Red — errors / failures ──
  ["失败原因", "kw-red"],
  ["错误", "kw-red"],
  ["失败", "kw-red"],
  ["报错", "kw-red"],
  ["异常", "kw-red"],
  ["崩溃", "kw-red"],
  ["出错", "kw-red"],
  ["无效", "kw-red"],
  ["拒绝", "kw-red"],
  ["超时", "kw-red"],
  ["找不到", "kw-red"],
  ["无法", "kw-red"],
  ["exception", "kw-red"],
  ["failed", "kw-red"],
  ["error", "kw-red"],
  ["timeout", "kw-red"],
  ["denied", "kw-red"],
  ["crash", "kw-red"],

  // ── Orange — warnings / attention ──
  ["请检查", "kw-orange"],
  ["请确认", "kw-orange"],
  ["请核实", "kw-orange"],
  ["即将过期", "kw-orange"],
  ["不推荐", "kw-orange"],
  ["已弃用", "kw-orange"],
  ["待确认", "kw-orange"],
  ["警告", "kw-orange"],
  ["注意", "kw-orange"],
  ["小心", "kw-orange"],
  ["风险", "kw-orange"],
  ["谨慎", "kw-orange"],
  ["过时", "kw-orange"],
  ["warning", "kw-orange"],
  ["caution", "kw-orange"],
  ["deprecated", "kw-orange"],
];

/** Regions that must never be keyword-highlighted (code, links, math, images). */
const HL_PROTECTED =
  /(<pre>[\s\S]*?<\/pre>|<code>[\s\S]*?<\/code>|<a\b[^>]*>[\s\S]*?<\/a>|<img\b[^>]*\/?>|<span class="katex[^"]*"[^>]*>[\s\S]*?<\/span>|<div class="katex-display"[^>]*>[\s\S]*?<\/div>)/g;

function escapeRegExp(str: string): string {
  return str.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Allow-list for markdown link/image URLs. Anything else (javascript:,
 * data:, vbscript: …) is left as escaped plain text instead of being
 * rendered as a clickable href/src.
 */
const SAFE_URL_SCHEME = /^(https?:\/\/|ozfile:\/\/|asset:\/\/)/i;

function isSafeUrl(url: string): boolean {
  return SAFE_URL_SCHEME.test(url);
}

function buildKeywordRegex(word: string): RegExp {
  // Pure-CJK terms sit between other CJK chars without separators, so no
  // word boundaries — instead capture an optional negation prefix and let
  // the caller skip the match (没有问题 / 无错误 / 不是错误 … would invert
  // the meaning). Plain capturing groups keep this compatible with older
  // WKWebView (lookbehind needs Safari 16.4+).
  if (/^[\u4e00-\u9fff]+$/.test(word)) {
    return new RegExp(`(无|没|不|未|非|不是|未能)?(${escapeRegExp(word)})`, "g");
  }
  return new RegExp(`\\b${escapeRegExp(word)}\\b`, "gi");
}

// Keyword regexes are compiled once at module load. `applyKeywordHighlights`
// used to call `buildKeywordRegex` for every keyword on every render —
// ~80 `RegExp` constructions per markdown pass.
type CompiledKeywordRule = { regex: RegExp; cls: string };
const COMPILED_KEYWORD_RULES: CompiledKeywordRule[] = KEYWORD_HIGHLIGHTS.map(
  ([word, cls]) => ({ regex: buildKeywordRegex(word), cls }),
);

/**
 * Wrap keyword matches in colored spans. Protected regions (code blocks,
 * inline code, links, KaTeX, images) are split out untouched; only plain
 * text segments are scanned. Matched spans are parked in %%KWPHn%% tokens
 * so later rules can never match inside an already-highlighted span, then
 * restored before returning.
 */
function applyKeywordHighlights(html: string): string {
  const tokens: string[] = [];
  html = html
    .split(HL_PROTECTED)
    .map((seg, i) => {
      if (i % 2 === 1) return seg;
      for (const { regex, cls } of COMPILED_KEYWORD_RULES) {
        seg = seg.replace(regex, (m: string, neg: string, kw: string) => {
          if (neg) return m;
          tokens.push(`<span class="${cls}">${kw}</span>`);
          return `%%KWPH${tokens.length - 1}%%`;
        });
      }
      return seg;
    })
    .join("");
  return html.replace(/%%KWPH(\d+)%%/g, (_m: string, idx: string) => tokens[parseInt(idx, 10)] ?? "");
}

// ── Rendered-HTML cache (T3.3) ─────────────────────────────────────
// Markdown rendering is deterministic per (text, highlight) pair.
// Historical assistant bubbles can re-render on unrelated store updates,
// so cache recent outputs keyed by a cheap hash of the source text. The
// cache is intentionally small and verifies the full text on hit, so a
// hash collision can only cost a miss, never a wrong render.
const MARKDOWN_CACHE_MAX = 256;
// Byte budget on top of the entry count: 256 entries x a 100KB code dump
// would pin tens of MB of strings in a 7x24 session.
const MARKDOWN_CACHE_MAX_BYTES = 8 * 1024 * 1024;

interface MarkdownCacheEntry {
  text: string;
  highlight: boolean;
  html: string;
}

const markdownRenderCache = new Map<number, MarkdownCacheEntry>();
let markdownCacheBytes = 0;

function entryBytes(e: MarkdownCacheEntry): number {
  return e.text.length + e.html.length;
}

function evictMarkdownCache() {
  while (
    markdownRenderCache.size > 0 &&
    (markdownRenderCache.size >= MARKDOWN_CACHE_MAX ||
      markdownCacheBytes > MARKDOWN_CACHE_MAX_BYTES)
  ) {
    const oldestKey = markdownRenderCache.keys().next().value;
    if (oldestKey === undefined) break;
    const oldest = markdownRenderCache.get(oldestKey);
    if (oldest) markdownCacheBytes -= entryBytes(oldest);
    markdownRenderCache.delete(oldestKey);
  }
}

function hashMarkdownText(text: string): number {
  // FNV-1a — fast, good enough for a verifying cache key.
  let hash = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    hash ^= text.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  return hash >>> 0;
}

export function renderMarkdown(text: string, opts?: { highlight?: boolean }): string {
  if (!text) return "";
  const highlight = opts?.highlight !== false;
  const originalText = text;
  const hash = hashMarkdownText(text);
  const cached = markdownRenderCache.get(hash);
  if (cached && cached.text === originalText && cached.highlight === highlight) {
    // LRU: refresh insertion order so hot entries are not evicted by age.
    markdownRenderCache.delete(hash);
    markdownRenderCache.set(hash, cached);
    return cached.html;
  }

  // Strip internal model-output tags that should never render. The
  // stream parser usually routes these to the reasoning channel, but
  // any that leak into text (e.g. historical sessions loaded from
  // disk) must be dropped before we render. Order matters: extract
  // <summary> first so the body survives, then drop the rest.
  //
  // IMPORTANT: We also strip STRAY closing/opening tags (e.g. a
  // `</summary>` without a matching `<summary>`) because:
  //   - Historical sessions may have the tag split across multiple
  //     text parts when the stream parser re-merged them.
  //   - The backend's `strip_summary_tags` only runs on the FINAL
  //     response, so partial streaming deltas that get persisted
  //     can leave a bare `</summary>` in the text.
  // Without this fallback, users see raw `</summary>` in the chat
  // (reported bug on 2026-06-21).
  text = text.replace(/<summary>[\s\S]*?<\/summary>/gi, "").trim();
  // Stray closing tag (no matching open) — just drop it.
  text = text.replace(/<\/summary\s*>/gi, "");
  // Stray opening tag (no matching close) — drop the tag, keep content.
  text = text.replace(/<summary\s*>/gi, "");
  text = text.replace(/<\/?respond>/gi, "");
  // Stray respond tags too.
  text = text.replace(/<\/?respond\s*>/gi, "");
  text = text.replace(/<antThinking[^>]*>[\s\S]*?<\/antThinking>/gi, "");
  text = text.replace(/<antThinking[^>]*\/>/gi, "");
  // Stray antThinking closing tag.
  text = text.replace(/<\/antThinking\s*>/gi, "");
  text = text.replace(/<thinking[^>]*>[\s\S]*?<\/thinking>/gi, "");
  text = text.replace(/<thinking[^>]*\/>/gi, "");
  text = text.replace(/<\/thinking\s*>/gi, "");
  text = text.replace(/<tool_code[^>]*>[\s\S]*?<\/tool_code>/gi, "");
  text = text.replace(/<tool_code[^>]*\/>/gi, "");
  text = text.replace(/<\/tool_code\s*>/gi, "");
  // Also strip stray XML-style Qwen3-Coder tool call fragments if
  // they leak into text (model occasionally emits `<tool_call>` inside
  // thinking blocks before the parser catches up — see QwenLM/Qwen3.6
  // issue #125).
  text = text.replace(/<\/?function[^>]*>/gi, "");
  text = text.replace(/<\/?parameter[^>]*>/gi, "");

  let html = text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    // Double quotes are not touched by the entity escapes above; escape
    // them too so user/agent content can never break out of an attribute
    // value we interpolate it into below (href/src). Renders identically.
    .replace(/"/g, "&quot;");

  html = html.replace(
    /```(\w*)\n?([\s\S]*?)```/g,
    (_m: string, _lang: string, code: string) => {
      return `<pre><code>${code.trim()}</code></pre>`;
    },
  );

  html = html.replace(/^##### (.+)$/gm, "<h5>$1</h5>");
  html = html.replace(/^#### (.+)$/gm, "<h4>$1</h4>");
  html = html.replace(/^### (.+)$/gm, "<h3>$1</h3>");
  html = html.replace(/^## (.+)$/gm, "<h2>$1</h2>");
  html = html.replace(/^# (.+)$/gm, "<h1>$1</h1>");

  html = html.replace(/^(?:[-*_]){3,}\s*$/gm, "<hr>");

  html = html.replace(/^&gt; (.+)$/gm, "<blockquote>$1</blockquote>");
  html = html.replace(/<\/blockquote>\n<blockquote>/g, "\n");

  html = html.replace(
    /(?:^- \[[ x]\] .+(?:\n|$))+/gm,
    (match) => `<ul>${match.replace(/^- \[([ x])\] (.+)$/gm, (_m: string, checked: string, item: string) => {
      const attr = checked === "x" ? ' checked=""' : "";
      return `<li><input type="checkbox" disabled${attr}> ${item}</li>`;
    })}</ul>`,
  );

  html = html.replace(
    /(?:^- .+(?:\n|$))+/gm,
    (match) => `<ul>${match.replace(/^- (.+)$/gm, "<li>$1</li>")}</ul>`,
  );

  html = html.replace(
    /(?:^\d+\. .+(?:\n|$))+/gm,
    (match) => `<ol>${match.replace(/^\d+\. (.+)$/gm, "<li>$1</li>")}</ol>`,
  );

  html = html.replace(
    /^\|(.+)\|\n\|([-| :]+)\|\n((?:\|.+\|\n?)+)/gm,
    (_m: string, head: string, _sep: string, body: string) => {
      const headers = head.split("|").map((h: string) => `<th>${h.trim()}</th>`).join("");
      const rows = body.trim().split("\n").map((row: string) => {
        const cells = row.split("|");
        return `<tr>${cells.slice(1, -1).map((c: string) => `<td>${c.trim()}</td>`).join("")}</tr>`;
      }).join("");
      return `<table><thead><tr>${headers}</tr></thead><tbody>${rows}</tbody></table>`;
    },
  );

  html = html.replace(/`([^`]+)`/g, "<code>$1</code>");

  function mathGuard(inner: string, offset: number, full: string): boolean {
    if (/<[a-z]/.test(inner)) return false;
    const before = full.slice(0, offset);
    if ((before.match(/<code>/g) || []).length > (before.match(/<\/code>/g) || []).length) return false;
    if ((before.match(/<pre>/g) || []).length > (before.match(/<\/pre>/g) || []).length) return false;
    return true;
  }

  html = html.replace(
    /\$\$([\s\S]+?)\$\$/g,
    (_m: string, inner: string, _offset: number, _full: string) =>
      renderMath(inner, true),
  );

  html = html.replace(
    /\$(.+?)\$/g,
    (_m: string, inner: string, offset: number, full: string) => {
      if (!mathGuard(inner, offset, full)) return _m;
      return renderMath(inner, false);
    },
  );

  html = html.replace(
    /\[([^\]]+)\]\(((?:[^\s)]|\([^\s()]*\))+)\)/g,
    (_m: string, text: string, url: string) => {
      if (!isSafeUrl(url)) return _m; // leave as escaped plain text
      return `<a href="${url}" target="_blank" rel="noopener noreferrer" class="md-link">${text}</a>`;
    },
  );

  html = html.replace(
    /!\[([^\]]*)\]\(([^)]+)\)/g,
    (_m: string, alt: string, url: string) => {
      if (!isSafeUrl(url)) return _m; // leave as escaped plain text
      return `<img src="${url}" alt="${alt}" loading="lazy">`;
    },
  );

  html = html.replace(/~~(.+?)~~/g, "<del>$1</del>");

  html = html.replace(/\*\*(.+?)\*\*/g, "<strong>$1</strong>");

  html = html.replace(/\*(.+?)\*/g, "<em>$1</em>");

  html = html.replace(
    /https?:\/\/[^\s<>"`]{2,}/g,
    (match: string, offset: number, full: string) => {
      if (full.includes(`<a href="${match}"`)) return match;
      const before = full.slice(0, offset);
      if ((before.match(/<code>/g) || []).length > (before.match(/<\/code>/g) || []).length) return match;
      if ((before.match(/<pre>/g) || []).length > (before.match(/<\/pre>/g) || []).length) return match;
      return `<a href="${match}" target="_blank" rel="noopener noreferrer" class="md-link">${match}</a>`;
    },
  );

  // Semantic keyword highlighting (skipped for user bubbles / file viewers).
  if (opts?.highlight !== false) {
    html = applyKeywordHighlights(html);
  }

  const placeholders: string[] = [];
  html = html.replace(
    /<(span\s+class="katex[^"]*"|div\s+class="katex-display")\b[^>]*>[\s\S]*?<\/\1>/g,
    (match) => {
      placeholders.push(match);
      return "%%PH" + (placeholders.length - 1) + "%%";
    },
  );
  const paragraphs = html.split(/\n\n+/);
  html = paragraphs
    .map((p) => {
      p = p.trim();
      if (!p) return "";
      if (/^<(?:h[1-5]|hr|ul|ol|li|blockquote)/.test(p) || p.startsWith("%%PH")) return p;
      return `<p>${p.replace(/\n/g, "<br>")}</p>`;
    })
    .join("");
  html = html.replace(/%%PH(\d+)%%/g, (_m: string, idx: string) => placeholders[parseInt(idx)] ?? "");

  // Replacing an existing (verified-mismatch) entry must first release
  // its byte share; then evict to budget and insert.
  const prev = markdownRenderCache.get(hash);
  if (prev) markdownCacheBytes -= entryBytes(prev);
  evictMarkdownCache();
  const entry: MarkdownCacheEntry = { text: originalText, highlight, html };
  markdownRenderCache.set(hash, entry);
  markdownCacheBytes += entryBytes(entry);

  return html;
}

function renderMath(latex: string, displayMode: boolean): string {
  try {
    return katex.renderToString(latex, {
      displayMode,
      throwOnError: false,
      output: "html",
    });
  } catch {
    return displayMode
      ? `<div class="math-block">${escapeHtml(latex)}</div>`
      : `<span class="math-inline">${escapeHtml(latex)}</span>`;
  }
}

/**
 * Lightweight renderer for LIVE streaming fragments. During streaming we
 * deliberately avoid the full `renderMarkdown` pipeline (block parsing,
 * table/list grouping, katex) because re-rendering the whole accumulated
 * text on every delta is O(n²) in reply length. This renderer only:
 *   1. escapes HTML, then
 *   2. applies inline formats (code, bold, italic, links) on the fly.
 * Block-level syntax (code fences, headings, lists) is left as plain
 * text while streaming; the `text_end` handoff re-renders the full text
 * with `renderMarkdown` for the precise final result.
 */
export function renderStreamingFragment(fragment: string): string {
  if (!fragment) return "";
  let html = escapeHtml(fragment);
  html = html.replace(/`([^`]+)`/g, "<code>$1</code>");
  html = html.replace(/\*\*(.+?)\*\*/g, "<strong>$1</strong>");
  html = html.replace(/\*(.+?)\*/g, "<em>$1</em>");
  html = html.replace(
    /https?:\/\/[^\s<>"`]{2,}/g,
    (match: string) =>
      `<a href="${match}" target="_blank" rel="noopener noreferrer" class="md-link">${match}</a>`,
  );
  html = applyKeywordHighlights(html);
  html = html.replace(/\n/g, "<br>");
  return html;
}

function escapeHtml(str: string): string {
  return str.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}
