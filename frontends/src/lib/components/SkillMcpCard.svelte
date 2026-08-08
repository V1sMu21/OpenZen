<script lang="ts">
  import type { ToolCallInfo } from "../stores/types";
  import { t, locale, tSync } from "../i18n";
  let lang = $state("zh");
  $effect(() => { lang = $locale; });

  let {
    toolCall = null as ToolCallInfo | null,
    durationMs = $bindable<number | undefined>(undefined),
    completed = false,
    showTimer = $bindable(false),
  } = $props();

  let collapsed = $state(true);
  let paramsExpanded = $state(false);
  let resultExpanded = $state(false);

  function toggle() {
    collapsed = !collapsed;
  }

  function formatArgs(args: string): string {
    try {
      return JSON.stringify(JSON.parse(args), null, 2);
    } catch {
      return args;
    }
  }

  function formatResult(result: unknown): string {
    if (result == null) return "";
    if (typeof result === "string") {
      try {
        return JSON.stringify(JSON.parse(result), null, 2);
      } catch {
        return result;
      }
    }
    try {
      return JSON.stringify(result, null, 2);
    } catch {
      return String(result);
    }
  }

  /** 结果 JSON 对象 → 逐 key 行 (与参数区同形式, 前 3 行折叠) */
  function resultRows(result: unknown): { key: string; value: string }[] | null {
    if (result == null) return null;
    let obj: unknown = result;
    if (typeof result === "string") {
      try { obj = JSON.parse(result); } catch { return null; }
    }
    if (!obj || typeof obj !== "object" || Array.isArray(obj)) return null;
    const keys = Object.keys(obj as Record<string, unknown>);
    if (keys.length === 0) return null;
    const rows: { key: string; value: string }[] = [];
    for (const k of keys) {
      const v = (obj as Record<string, unknown>)[k];
      if (v == null) continue;
      const value =
        typeof v === "string"
          ? truncate(v, 100)
          : truncate(JSON.stringify(v), 100);
      rows.push({ key: paramLabel(k), value });
    }
    return rows.length > 0 ? rows : null;
  }

  function formatDuration(ms: number | undefined): string {
    if (ms == null) return "";
    if (!Number.isFinite(ms) || ms < 0) return "";
    if (ms < 1000) return `${Math.round(ms)}${tSync(lang, "message.duration.ms")}`;
    const totalSec = Math.floor(ms / 1000);
    if (totalSec < 60) return tSync(lang, "message.duration.formatSec").replace("{s}", (ms / 1000).toFixed(1));
    const m = Math.floor(totalSec / 60);
    const s = totalSec % 60;
    return tSync(lang, "message.duration.formatShort")
      .replace("{m}", String(m))
      .replace("{s}", String(s));
  }

  /**
   * Categorize a tool invocation as a skill or MCP call and produce the
   * collapsed-card header info.
   *
    * MCP:        `mcp__<server>__<tool>` → { kind: 'mcp', server, tool }
    * Skill:      `skill_mcp_search`/`list` whose result mentions a specific
    *             skill → { kind: 'skill', name }
    * Skill/MCP:  `skill_mcp_search`/`list` with empty/no-match result →
    *             { kind: 'skill_mcp', name } (still rendered so the call is
    *             visible to the user even when the registry has nothing to offer)
    * Other:      anything else → { kind: 'other', name } (not rendered here)
    */
  type Categorized =
    | { kind: "mcp"; server: string; tool: string }
    | { kind: "skill"; name: string; query?: string }
    | { kind: "skill_mcp"; name: string; query?: string }
    | { kind: "other"; name: string };

  function categorize(name: string, args: string, result?: string): Categorized {
    if (name.startsWith("mcp__")) {
      const rest = name.slice("mcp__".length);
      const sep = rest.indexOf("__");
      if (sep > 0) {
        return { kind: "mcp", server: rest.slice(0, sep), tool: rest.slice(sep + 2) };
      }
      return { kind: "mcp", server: rest, tool: "" };
    }
    if (name === "skill_mcp_search" || name === "skill_mcp_list") {
      let q: string | undefined;
      try {
        const parsed = JSON.parse(args);
        q = typeof parsed.query === "string" ? parsed.query : undefined;
      } catch { /* ignore */ }
      const matched = firstSkillNameFromResult(result);
      if (matched) return { kind: "skill", name: matched, query: q };
      return { kind: "skill_mcp", name, query: q };
    }
    return { kind: "other", name };
  }

  function firstSkillNameFromResult(result: string | undefined): string | null {
    if (!result) return null;
    // Primary path: skill_mcp_search's ToolOutput.data is a JSON object
    // of shape { "results": [{ "type": "skill", "name": "...", ... }, ...] }.
    // Parse it and return the first skill's name.
    try {
      const parsed = JSON.parse(result) as {
        results?: Array<{ type?: string; name?: string }>;
      };
      if (parsed && Array.isArray(parsed.results)) {
        for (const r of parsed.results) {
          if (r && r.type === "skill" && typeof r.name === "string" && r.name.length > 0) {
            return r.name;
          }
        }
      }
    } catch {
      /* fall through to markdown regex */
    }
    // Fallback: the tool's `prompt` field renders as markdown lines like
    //   - [skill] **<name>** — <desc> (quality: 0.85)
    // Pick the first such line and extract the name.
    const lines = result.split("\n");
    for (const line of lines) {
      const m = line.match(/^#{1,4}\s*Skill:\s*(.+?)\s*$/i)
        ?? line.match(/^-\s*\[skill\]\s*\*\*([^*]+)\*\*/i);
      if (m && m[1]) return m[1].trim();
    }
    return null;
  }

  function summaryArg(args: string): string {
    if (!args || args === "{}") return "";
    try {
      const parsed = JSON.parse(args);
      const v = parsed.query ?? parsed.path ?? parsed.url ?? parsed.prompt;
      if (typeof v === "string") {
        return v.length > 48 ? v.slice(0, 45) + "..." : v;
      }
    } catch { /* ignore */ }
    return "";
  }

  /** 参数 key 的 i18n 标签: 有 tool.param.<key> 翻译则用之, 否则保留原 key */
  function paramLabel(key: string): string {
    const tkey = `tool.param.${key}`;
    const label = tSync(lang, tkey);
    return label === tkey ? key : label;
  }

  function truncate(s: string, max: number): string {
    return s.length > max ? s.slice(0, max - 3) + "..." : s;
  }

  /** 展开体第一行起: 参数逐 key 行 — file_path/path 恒第一行(key 显示
   *  path), content 其次, priority 再后, 其余按参数原序. 最多显示 3 行,
   *  多余折叠点击展开 (与 ToolCallCard 同一形式). */
  const MAX_PARAM_ROWS = 3;

  function paramRows(args: string): { key: string; value: string }[] {
    if (!args || args === "{}") return [];
    let parsed: Record<string, unknown>;
    try {
      parsed = JSON.parse(args);
    } catch { return []; }
    if (!parsed || typeof parsed !== "object") return [];
    const rank = (k: string): number => {
      if (k === "file_path" || k === "path") return 0;
      if (k === "content") return 1;
      if (k === "priority") return 2;
      if (k === "status" || k === "new_status") return 3;
      if (k === "key") return 4;
      return 10;
    };
    const keys = Object.keys(parsed).sort((a, b) => rank(a) - rank(b));
    const rows: { key: string; value: string }[] = [];
    for (const k of keys) {
      const v = parsed[k];
      if (v == null || v === "") continue;
      const isPath = k === "file_path" || k === "path";
      const label = paramLabel(isPath ? "path" : k);
      const value =
        typeof v === "string"
          ? truncate(v, 100)
          : truncate(JSON.stringify(v), 100);
      rows.push({ key: label, value });
    }
    return rows;
  }

  // ── Running timer (same approach as ToolCallCard) ──
  let liveRunningMs = $state(0);
  let timerStart = $state.raw(0);
  let argsSettled = $state(false);
  let isRunning = $derived(!toolCall?.result && !completed);
  let settleTimeout: ReturnType<typeof setTimeout> | null = null;

  $effect(() => {
    const args = toolCall?.arguments;
    if (!isRunning) {
      if (settleTimeout !== null) {
        clearTimeout(settleTimeout);
        settleTimeout = null;
      }
      if (argsSettled) argsSettled = false;
      return;
    }
    if (settleTimeout !== null) clearTimeout(settleTimeout);
    settleTimeout = setTimeout(() => {
      settleTimeout = null;
      if (isRunning && !argsSettled) {
        argsSettled = true;
        if (timerStart === 0) timerStart = Date.now();
      }
    }, 400);
    return () => {
      if (settleTimeout !== null) {
        clearTimeout(settleTimeout);
        settleTimeout = null;
      }
    };
  });

  $effect(() => {
    if (!isRunning) {
      if (durationMs != null) liveRunningMs = durationMs;
      timerStart = 0;
      if (argsSettled) argsSettled = false;
      return;
    }
    if (argsSettled && timerStart === 0) timerStart = Date.now();
  });

  $effect(() => {
    if (timerStart === 0) return;
    const id = setInterval(() => {
      liveRunningMs = Date.now() - timerStart;
    }, 200);
    return () => clearInterval(id);
  });

  let durationLabel = $derived(
    isRunning
      ? (argsSettled ? formatDuration(liveRunningMs) : "…")
      : formatDuration(durationMs ?? liveRunningMs)
  );

  let category = $derived(
    toolCall ? categorize(toolCall.name, toolCall.arguments, typeof toolCall.result === "string" ? toolCall.result : undefined) : { kind: "other" as const, name: "" }
  );
  let headerTitle = $derived.by(() => {
    if (category.kind === "mcp") {
      return `MCP · ${category.server}${category.tool ? " / " + category.tool : ""}`;
    }
    if (category.kind === "skill") {
      return `Skill · ${category.name}`;
    }
    if (category.kind === "skill_mcp") {
      return `Skill/MCP · ${category.name}${category.query ? " · " + category.query : ""}`;
    }
    return toolCall?.name ?? "";
  });
  let headerIcon = $derived(
    category.kind === "mcp" ? "◇" :
    category.kind === "skill" ? "★" :
    category.kind === "skill_mcp" ? "☆" :
    "⏵"
  );
</script>

{#if toolCall && (category.kind === "mcp" || category.kind === "skill" || category.kind === "skill_mcp")}
  <div class="smc">
    <button class="smc-header" onclick={toggle}>
      <span class="smc-icon" aria-hidden="true">{headerIcon}</span>
      <span class="smc-title">{headerTitle}</span>
      {#if (category.kind === "mcp" || category.kind === "skill_mcp") && toolCall.arguments && toolCall.arguments !== "{}"}
        <span class="smc-arg">{summaryArg(toolCall.arguments)}</span>
      {/if}
            {#if durationLabel && showTimer}
              <span class="smc-duration" title="Execution time">{durationLabel}</span>
            {/if}
      {#if toolCall.result || completed}
        <span class="smc-status done">已竟</span>
      {:else}
        <span class="smc-status running">{$t("tool.running")}</span>
      {/if}
    </button>
    {#if !collapsed}
      {@const rows = paramRows(toolCall.arguments)}
      <div class="smc-body">
        {#each rows.slice(0, MAX_PARAM_ROWS) as row (row.key)}
          <div class="smc-line"><span class="k">{row.key}</span><span class="v">{row.value}</span></div>
        {/each}
        {#if rows.length > MAX_PARAM_ROWS}
          <button class="smc-more" onclick={() => paramsExpanded = !paramsExpanded}>
            {paramsExpanded
              ? `▾ ${$t("tool.collapse")}`
              : `▸ ${$t("tool.moreParams").replace("{n}", String(rows.length - MAX_PARAM_ROWS))}`}
          </button>
          {#if paramsExpanded}
            {#each rows.slice(MAX_PARAM_ROWS) as row (row.key)}
              <div class="smc-line"><span class="k">{row.key}</span><span class="v">{row.value}</span></div>
            {/each}
          {/if}
        {/if}
        {#if toolCall.result}
          <div class="smc-section">
            <div class="smc-label">{$t("tool.result")}</div>
            {#if resultRows(toolCall.result) != null}
              {@const rrows = resultRows(toolCall.result)!}
              {#if resultExpanded}
                {#each rrows as r (r.key)}
                  <div class="smc-line"><span class="k">{r.key}</span><span class="v">{r.value}</span></div>
                {/each}
              {:else}
                {#each rrows.slice(0, MAX_PARAM_ROWS) as r (r.key)}
                  <div class="smc-line"><span class="k">{r.key}</span><span class="v">{r.value}</span></div>
                {/each}
                {#if rrows.length > MAX_PARAM_ROWS}
                  <button class="smc-more" onclick={() => resultExpanded = !resultExpanded}>
                    {resultExpanded
                      ? `▾ ${$t("tool.collapse")}`
                      : `▸ ${$t("tool.moreParams").replace("{n}", String(rrows.length - MAX_PARAM_ROWS))}`}
                  </button>
                {/if}
              {/if}
            {:else}
              <pre class="smc-code smc-result">{formatResult(toolCall.result)}</pre>
            {/if}
          </div>
        {/if}
      </div>
    {/if}
  </div>
{/if}

<style>
  .smc {
    /* 与 ToolCallCard 同一形式: 无边框, 仅下缘青线 */
    margin: 2px 0;
    border-bottom: 1px solid var(--color-hairline);
    border-radius: 0;
    background: none;
    overflow: hidden;
  }

  .smc-header {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 5px 8px;
    background: none;
    border: none;
    color: var(--color-body);
    cursor: pointer;
    font-family: var(--font-sans);
    font-size: 12px;
    text-align: left;
    transition: background 0.1s;
  }

  .smc-header:hover {
    background: color-mix(in srgb, var(--color-accent) 10%, transparent);
  }

  .smc-icon {
    flex-shrink: 0;
    font-size: 13px;
    line-height: 1;
    color: var(--color-accent);
    width: 14px;
    text-align: center;
  }

  .smc-title {
    font-weight: 500;
    color: var(--color-ink);
    flex-shrink: 0;
    font-family: var(--font-mono);
    font-size: 11.5px;
  }

  .smc-arg {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--color-primary);
    font-family: var(--font-mono);
    font-size: 11px;
    opacity: 0.85;
    min-width: 0;
    flex: 1;
  }

  .smc-duration {
    flex-shrink: 0;
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--color-dim);
    font-weight: 500;
    font-variant-numeric: tabular-nums;
  }

  .smc-status {
    flex-shrink: 0;
    font-size: 10px;
    padding: 1px 6px;
    border-radius: 4px;
    font-weight: 500;
    margin-left: auto;
  }

  .smc-status.done {
    background: none;
    color: var(--color-dim);
  }

  .smc-status.running {
    background: color-mix(in srgb, #f59e0b 18%, transparent);
    color: #f59e0b;
  }

  .smc-body {
    padding: 6px 8px 8px;
    font-family: var(--font-mono);
  }

  /* 主参数行 (path/query/url): 与 ToolCallCard 三段式一致 */
  .smc-line {
    display: flex;
    align-items: baseline;
    gap: 8px;
    white-space: nowrap;
    overflow: hidden;
    font-family: var(--font-mono);
    font-size: 11px;
    padding: 2px 2px 6px;
  }
  .smc-line .k {
    color: var(--color-primary);
    opacity: 0.7;
    flex: none;
  }
  .smc-line .v {
    color: var(--color-ink);
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .smc-section {
    margin-top: 6px;
  }
  .smc-section:first-child {
    margin-top: 0;
  }

  /* 参数折叠按钮: 超出 3 行 → 点击展开其余 key (与 ToolCallCard 同形式) */
  .smc-more {
    margin-top: 6px;
    padding: 1px 8px;
    background: none;
    border: 1px dashed var(--color-hairline);
    border-radius: 2px;
    color: var(--color-primary);
    font: inherit;
    font-size: 10.5px;
    cursor: pointer;
    opacity: 0.85;
  }
  .smc-more:hover {
    opacity: 1;
  }

  .smc-label {
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--color-dim);
    margin-bottom: 3px;
  }

  .smc-code {
    font-family: var(--font-mono);
    font-size: 11px;
    line-height: 1.45;
    color: var(--color-code-text);
    background: var(--color-code-bg);
    border-radius: 4px;
    padding: 5px 7px;
    overflow-x: auto;
    white-space: pre-wrap;
    word-break: break-all;
    margin: 0;
    max-height: 200px;
    overflow-y: auto;
  }

  .smc-result {
    max-height: 240px;
  }
</style>
