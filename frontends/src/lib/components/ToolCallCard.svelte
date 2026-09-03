<script lang="ts">
  import type { ToolCallInfo } from "../stores/types";
  import { t, locale, tSync } from "../i18n";
  import { isTauri, tauriInvoke } from "../api/tauri";
  let lang = $state("zh");
  $effect(() => { lang = $locale; });

  let {
    toolCall = null as ToolCallInfo | null,
    repeatCount = undefined as number | undefined,
    durationMs = $bindable<number | undefined>(undefined),
    completed = false,
    showTimer = $bindable(false),
    hasError = false,
    workingDir = "",
  } = $props();

  let collapsed = $state(true);
  let cmdExpanded = $state(false);
  let resExpanded = $state(false);
  let paramsExpanded = $state(false);

  // computer_screenshot: fetch the PNG as a data URI (path-restricted IPC)
  // once the tool completes, for an inline preview inside the card.
  let shotUri = $state<string | null>(null);
  let shotFailed = $state(false);

  /** Parse a tool result string into an object; null on failure. Shared by
   *  the per-tool result extractors below. */
  function parseResultObj(result: unknown): Record<string, unknown> | null {
    if (typeof result !== "string") {
      return result && typeof result === "object"
        ? (result as Record<string, unknown>)
        : null;
    }
    try {
      const o = JSON.parse(result);
      return o && typeof o === "object" ? (o as Record<string, unknown>) : null;
    } catch {
      return null;
    }
  }
  $effect(() => {
    if (
      toolCall?.name !== "computer_screenshot"
      || !completed
      || !isTauri()
      || !workingDir
      || !toolCall.result
      || shotUri
      || shotFailed
    ) {
      return;
    }
    const r = parseResultObj(toolCall.result);
    if (!r || typeof r.path !== "string" || !r.path) {
      // no path in the result — nothing to preview (latched, like every exit)
      shotFailed = true;
      return;
    }
    {
      void (async () => {
        try {
          // Tauri 2 matches Rust snake_case params to camelCase JS keys.
          const res = await tauriInvoke("computer_screenshot_data", {
            path: r.path,
            workingDir,
          }) as { data_uri?: string };
          if (typeof res?.data_uri === "string") {
            shotUri = res.data_uri;
          } else {
            shotFailed = true;
          }
        } catch {
          shotFailed = true;
        }
      })();
    }
  });

  function toggle() {
    collapsed = !collapsed;
  }

  function formatDuration(ms: number | undefined): string {
    if (ms == null) return "";
    if (!Number.isFinite(ms) || ms < 0) return "";
    if (ms < 1000) return `${Math.round(ms)}${tSync(lang, "message.duration.ms")}`;
    const totalSec = Math.floor(ms / 1000);
    if (totalSec < 60) {
      return tSync(lang, "message.duration.formatSec").replace("{s}", (ms / 1000).toFixed(1));
    }
    const m = Math.floor(totalSec / 60);
    const s = totalSec % 60;
    return tSync(lang, "message.duration.format")
      .replace("{m}", String(m))
      .replace("{s}", String(s));
  }

  function summaryArg(args: string, name?: string, result?: string): string {
    // For todoupdate, prefer content from the result (tool echoes it back)
    if (name === "todoupdate" && result) {
      try {
        const r = JSON.parse(result);
        if (r.content && typeof r.content === "string" && r.content.trim()) {
          const c = r.content.trim();
          return c.length > 48 ? c.slice(0, 45) + "..." : c;
        }
      } catch { /* fall through to args */ }
    }
    if (!args || args === "{}") return "";
    try {
      const parsed = JSON.parse(args);
      const keys = ["file_path", "pattern", "path", "url", "content", "prompt", "data", "goal", "question", "query", "code", "name"];
      for (const k of keys) {
        const v = parsed[k];
        if (v && typeof v === "string") {
          return v.length > 48 ? v.slice(0, 45) + "..." : v;
        }
      }
      for (const v of Object.values(parsed)) {
        if (typeof v === "string") {
          const s = v as string;
          return s.length > 48 ? s.slice(0, 45) + "..." : s;
        }
      }
      return "";
    } catch {
      // Args still streaming (or truncated by the model): JSON.parse
      // fails but the card must not read as an empty "运行中…". Show the
      // streaming content itself — same key priority as above, else the
      // raw fragment.
      const keys = ["file_path", "pattern", "path", "url", "content", "prompt", "data", "goal", "question", "query", "code", "name"];
      for (const k of keys) {
        const v = extractStringField(args, k);
        if (v && v.trim()) return oneLine(v, 48);
      }
      return oneLine(args, 48);
    }
  }

  /** 运行类工具: 展开体显示 `cmd` 命令行 */
  const RUN_TOOLS = new Set(["code_run", "bash", "sh", "shell", "run_test", "exec"]);

  /** 待办工具: 参数区已含 内容/状态, 结果区只补 todo_id, 不重复 content */
  const TODO_TOOLS = new Set(["todowrite", "todoupdate"]);

  /** 搜索工具: 结果区直接显示文件/匹配列表, 而非 JSON */
  const SEARCH_TOOLS = new Set(["glob", "grep"]);

  /** 普通工具参数逐 key 行: 最多显示前 3 行, 多余折叠点击展开 */
  const MAX_PARAM_ROWS = 3;

  function parseArgs(args: string): Record<string, unknown> | null {
    if (!args || args === "{}") return null;
    try {
      const parsed = JSON.parse(args);
      return parsed && typeof parsed === "object" ? parsed : null;
    } catch {
      return null;
    }
  }

  /**
   * Extract a top-level string field from (possibly INCOMPLETE) JSON args.
   *
   * Tool args arrive as streamed fragments (`tool_input_delta`), so while a
   * long `code` value is still streaming the object can't be parsed and
   * `JSON.parse` fails — the run-code card then showed nothing but
   * "运行中…" for the whole args-streaming window (and forever when the
   * model truncated the JSON). This scanner reads `field` directly,
   * tolerating a missing closing quote / trailing escape, and unescapes
   * standard JSON escapes. Preview use only — never feed the result back
   * into the tool.
   */
  function extractStringField(raw: string, field: string): string | null {
    if (!raw) return null;
    const keyIdx = raw.indexOf(`"${field}"`);
    if (keyIdx < 0) return null;
    let i = raw.indexOf(":", keyIdx + field.length + 2);
    if (i < 0) return null;
    i++;
    while (i < raw.length && /\s/.test(raw[i])) i++;
    if (raw[i] !== '"') return null;
    i++;
    let out = "";
    while (i < raw.length) {
      const ch = raw[i];
      if (ch === "\\") {
        const next = raw[i + 1];
        if (next === undefined) break; // escape cut mid-stream
        if (next === "n") out += "\n";
        else if (next === "t") out += "\t";
        else if (next === "r") out += "\r";
        else if (next === "u") {
          const hex = raw.slice(i + 2, i + 6);
          if (hex.length < 4) break;
          out += String.fromCharCode(parseInt(hex, 16));
          i += 6;
          continue;
        } else out += next; // \" \\ \/ etc.
        i += 2;
        continue;
      }
      if (ch === '"') break; // closing quote — value complete
      out += ch;
      i++;
    }
    return out;
  }

  /** Collapse whitespace for one-line raw previews. */
  function oneLine(s: string, max: number): string {
    const flat = s.replace(/\s+/g, " ").trim();
    return flat.length > max ? flat.slice(0, max - 3) + "..." : flat;
  }

  function truncate(s: string, max: number): string {
    return s.length > max ? s.slice(0, max - 3) + "..." : s;
  }

  /** path 显示优化: 等于工作目录 → 只显示目录名; 是工作目录子路径
   *  → 显示相对路径; 其余原样. */
  function displayPath(p: string): string {
    if (!workingDir) return p;
    const base = workingDir.replace(/\/+$/, "");
    if (!base) return p;
    if (p === base) return p.split("/").pop() || p;
    if (p.startsWith(base + "/")) return p.slice(base.length + 1);
    return p;
  }

  /** 参数 key 的 i18n 标签: 有 tool.param.<key> 翻译则用之, 否则保留原 key */
  function paramLabel(key: string): string {
    const tkey = `tool.param.${key}`;
    const label = tSync(lang, tkey);
    return label === tkey ? key : label;
  }

  /** 运行类工具: path 行 — 有 file_path 显示相对路径, 否则显示工作目录 */
  function mainArg(args: string, name: string): { key: string; value: string } | null {
    if (!RUN_TOOLS.has(name)) return null;
    const parsed = parseArgs(args);
    if (!parsed) {
      // Partial-args fallback while the JSON is still streaming.
      const p = extractStringField(args, "file_path") ?? extractStringField(args, "path");
      if (p && p.trim()) return { key: paramLabel("path"), value: truncate(displayPath(p), 60) };
      if (workingDir) return { key: paramLabel("path"), value: truncate(displayPath(workingDir), 60) };
      return null;
    }
    const p = parsed["file_path"] ?? parsed["path"];
    if (typeof p === "string" && p.trim()) return { key: paramLabel("path"), value: truncate(displayPath(p), 60) };
    if (workingDir) return { key: paramLabel("path"), value: truncate(displayPath(workingDir), 60) };
    return null;
  }

  /** 普通工具参数逐 key 行: file_path/path 恒第一行(key 显示 path),
   *  content 其次, priority 再后, status/new_status 其后, key 字段再后,
   *  其余按参数原序. */
  function paramRows(args: string): { key: string; value: string }[] {
    const parsed = parseArgs(args);
    if (!parsed) {
      // Partial-args fallback: show the streaming fragment as one raw row
      // instead of an empty parameter block.
      if (args && args !== "{}") {
        return [{ key: "args", value: oneLine(args, 100) }];
      }
      return [];
    }
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
          ? (isPath ? displayPath(v) : truncate(v, 100))
          : truncate(JSON.stringify(v), 100);
      rows.push({ key: label, value });
    }
    return rows;
  }

  /** read 等工具的结果: 提取 content 字段, 避免显示 JSON {} */
  function resultContent(result: unknown): string | null {
    if (result == null) return null;
    let obj: unknown = result;
    if (typeof result === "string") {
      try { obj = JSON.parse(result); } catch { return null; }
    }
    if (obj && typeof obj === "object" && "content" in obj) {
      const c = (obj as Record<string, unknown>)["content"];
      if (typeof c === "string" && c.trim()) return c.trim();
    }
    return null;
  }

  /** 结果 JSON 对象 → 逐 key 行: 与参数区同一形式, 前 3 行折叠 */
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

  /** 搜索工具的结果: glob → 文件列表逐行; grep → file:line text 逐行 */
  function searchResultText(result: unknown): string | null {
    if (result == null) return null;
    let obj: unknown = result;
    if (typeof result === "string") {
      try { obj = JSON.parse(result); } catch { return null; }
    }
    if (!obj || typeof obj !== "object") return null;
    const r = obj as Record<string, unknown>;
    if (Array.isArray(r["files"])) {
      const files = (r["files"] as unknown[]).filter((f): f is string => typeof f === "string");
      if (files.length === 0) return null;
      return files.join("\n");
    }
    if (Array.isArray(r["matches"])) {
      const matches = r["matches"] as Record<string, unknown>[];
      if (matches.length === 0) return null;
      const lines: string[] = [];
      for (const m of matches) {
        const f = typeof m["file"] === "string" ? m["file"] : "";
        const ln = typeof m["line"] === "number" ? String(m["line"]) : "";
        const tx = typeof m["text"] === "string" ? m["text"] : "";
        lines.push(`${f}${ln ? ":" + ln : ""}${tx ? ": " + tx : ""}`);
      }
      return lines.join("\n");
    }
    return null;
  }

  /** 展开体第二行: 运行命令 (仅运行类工具). code_run/bash 的 `code`
   *  参数即命令本身; python 显示实际的 python3 调用形式.
   *  Args 仍在流式时 JSON 不可解析 — 直接从片段中抽取 code 值实时预览. */
  function cmdLine(args: string, name: string): string | null {
    if (!RUN_TOOLS.has(name)) return null;
    const parsed = parseArgs(args);
    if (parsed) {
      const code = parsed["code"];
      if (typeof code !== "string" || !code.trim()) return null;
      const type = typeof parsed["type"] === "string" ? parsed["type"] : "bash";
      if (type === "python" || type === "py") {
        return `python3 -X utf8 -u <<'PY'\n${code.trim()}\nPY`;
      }
      return code.trim();
    }
    const code = extractStringField(args, "code");
    if (!code || !code.trim()) return null;
    const type = extractStringField(args, "type") ?? "bash";
    if (type === "python" || type === "py") {
      return `python3 -X utf8 -u <<'PY'\n${code.trim()}\nPY`;
    }
    return code.trim();
  }

  /** 运行类工具的结果: {exit_code, stdout, stderr} 展开为可读文本,
   *  而不是一段 JSON. */
  function runResultText(result: unknown): string {
    if (result == null) return "";
    let obj: unknown = result;
    if (typeof result === "string") {
      try { obj = JSON.parse(result); } catch { return result; }
    }
    if (obj && typeof obj === "object" && "exit_code" in obj) {
      const r = obj as Record<string, unknown>;
      const lines: string[] = [`exit ${String(r["exit_code"])}`];
      if (typeof r["elapsed_secs"] === "number") lines[0] += ` · ${(r["elapsed_secs"] as number).toFixed(1)}s`;
      if (typeof r["output_file"] === "string" && r["output_file"]) lines.push(`output: ${r["output_file"]}`);
      if (typeof r["truncated_preview"] === "string" && r["truncated_preview"]) {
        lines.push(r["truncated_preview"]);
      } else if (typeof r["stdout"] === "string" && r["stdout"].trim()) {
        lines.push(r["stdout"].trimEnd());
      }
      if (typeof r["stderr"] === "string" && r["stderr"].trim()) {
        lines.push(`stderr: ${r["stderr"].trimEnd()}`);
      }
      return lines.join("\n");
    }
    return "";
  }

  /** 展开体第三行: 精简结果 (单行化 + 截断) */
  function shortResult(result: unknown): string {
    if (result == null) return "";
    let s: string;
    if (typeof result === "string") {
      s = result;
    } else {
      try { s = JSON.stringify(result); } catch { s = String(result); }
    }
    // 单行化 + 截断 160 字符
    const oneLine = s.replace(/\s+/g, " ").trim();
    return oneLine.length > 160 ? oneLine.slice(0, 157) + "..." : oneLine;
  }

  function toolLabel(name: string): string {
    // 通用 i18n: 有 toolname.<name> 翻译则用之, 否则保留原始工具名
    const tkey = `toolname.${name}`;
    const label = tSync(lang, tkey);
    return label === tkey ? name : label;
  }

  // ── Running timer ─────────────────────────────────────────────────
  //
  // The parent (ChatMessage.svelte) creates `toolCall` as a fresh inline
  // object on every render: `toolCall={{ name, arguments, result }}`. That
  // means the `toolCall` prop reference changes constantly while the
  // agent streams, so naively starting the timer in an $effect that reads
  // `toolCall` would reset the clock on every store update.
  //
  // We instead use `toolCall.name` + the presence of `toolCall.result` as
  // the *real* state transitions, and keep the start timestamp in
  // $state.raw so Svelte's deep-equality short-circuiting can't hide a
  // reset from us. A second $effect owns the interval so it only re-runs
  // when the timer genuinely starts/stops, not on every prop change.
  //
  // Bug #4: the live counter used to start at component mount
  // (i.e. when `tool_input_start` fired) but the final `durationMs`
  // from the backend measures the gap between `tool_input_available`
  // (args fully received) and `tool_output_available` (result
  // received). That meant the user could see "3.0s" while the tool
  // was running and then "2.7s" once it completed — a confusing
  // backwards jump. We now reset the start timestamp the moment the
  // `toolCall.arguments` text stops changing, which corresponds to
  // `tool_input_available` and is exactly what the backend's
  // durationMs measures. While the args are still streaming we
  // display a dash to make clear that the timer is in its "waiting
  // for args" phase, not silently counting toward the wrong total.

  let liveRunningMs = $state(0);
  let timerStart = $state.raw(0);
  let argsSettled = $state(false);
  let isRunning = $derived(!toolCall?.result && !completed);
  let settleTimeout: ReturnType<typeof setTimeout> | null = null;

  // Args-settling watchdog. When the args text changes (the parent
  // re-renders with a new fragment), cancel any pending settle
  // check and schedule a new one 400ms in the future. If args
  // still match by then, declare them settled and start the real
  // timer from that moment — the same span the backend's
  // durationMs measures (tool_input_available →
  // tool_output_available).
  $effect(() => {
    const args = toolCall?.arguments;
    // Read `isRunning` and `argsSettled` so this effect re-runs on
    // either transition, but DON'T write to them here.
    if (!isRunning) {
      if (settleTimeout !== null) {
        clearTimeout(settleTimeout);
        settleTimeout = null;
      }
      if (argsSettled) argsSettled = false;
      return;
    }
    // While running, wait for args to settle (no change for 400ms).
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

  // Start/stop the timer only on real transitions of `isRunning`.
  // Keying the effect on `isRunning` (a boolean) — not on `toolCall` —
  // means re-renders with the same running state don't touch the clock.
  $effect(() => {
    if (!isRunning) {
      // Tool finished or component finalized. Snap the live counter
      // to whatever the backend measured, so the displayed value
      // doesn't visibly jump backwards at completion (Bug #4).
      if (durationMs != null) {
        liveRunningMs = durationMs;
      }
      timerStart = 0;
      if (argsSettled) argsSettled = false;
      return;
    }
    if (argsSettled && timerStart === 0) {
      timerStart = Date.now();
    }
  });

  // Interval owner. Re-runs only when `timerStart` actually changes
  // (start with non-zero value, or reset to 0). The cleanup clears the
  // interval, so when `timerStart` flips to 0 the ticker stops cleanly.
  $effect(() => {
    if (timerStart === 0) return;
    const id = setInterval(() => {
      liveRunningMs = Date.now() - timerStart;
    }, 200);
    return () => clearInterval(id);
  });

  // Pick the right duration: static `durationMs` once finalized, live
  // counter while running. If we somehow have neither (e.g. just mounted
  // and the first tick hasn't fired), fall back to 0.
  let durationLabel = $derived(
    isRunning
      ? (argsSettled
          ? formatDuration(liveRunningMs)
          : "…")
      : formatDuration(durationMs ?? liveRunningMs)
  );
</script>

{#if toolCall}
  <div class="tcc" class:error={hasError}>
    <button class="tcc-header" onclick={toggle}>
      <span class="tcc-sym">◈</span>
      <span class="tcc-name">{toolLabel(toolCall.name)}</span>
      {#if repeatCount != null && repeatCount > 1}
        <span class="tcc-count">&times;{repeatCount}</span>
      {/if}
      {#if summaryArg(toolCall.arguments, toolCall.name, String(toolCall.result ?? ''))}
        <span class="tcc-arg">{summaryArg(toolCall.arguments, toolCall.name, String(toolCall.result ?? ''))}</span>
      {/if}
      {#if durationLabel && showTimer}
        <span class="tcc-duration" title="Execution time">{durationLabel}</span>
      {/if}
      {#if hasError}
        <span class="tcc-status err">朱砂 ✕</span>
      {:else if toolCall.result || completed}
        <span class="tcc-status done">已竟</span>
      {:else}
        <span class="tcc-status running"><span class="run-dot"></span><span>{$t("tool.running")}</span></span>
      {/if}
    </button>
    {#if !collapsed}
      {@const isRun = RUN_TOOLS.has(toolCall.name)}
      {@const isTodo = TODO_TOOLS.has(toolCall.name)}
      {@const isSearch = SEARCH_TOOLS.has(toolCall.name)}
      {@const rows = paramRows(toolCall.arguments ?? "")}
      {@const main = isRun ? mainArg(toolCall.arguments ?? "", toolCall.name) : null}
      {@const cmd = cmdLine(toolCall.arguments ?? "", toolCall.name)}
      {@const runRes = isRun ? runResultText(toolCall.result) : ""}
      {@const content = !isRun && !isTodo ? resultContent(toolCall.result) : null}
      {@const searchRes = isSearch ? searchResultText(toolCall.result) : null}
      {@const resRows = !isRun && !isTodo ? resultRows(toolCall.result) : null}
      {@const rowLimit = isSearch ? 1 : MAX_PARAM_ROWS}
      <div class="tcc-body">
        {#if isRun}
          {#if main}
            <div class="tcc-line"><span class="k">{main.key}</span><span class="v">{main.value}</span></div>
          {/if}
          {#if cmd}
            <button class="tcc-line tcc-cmd" class:expanded={cmdExpanded} onclick={() => cmdExpanded = !cmdExpanded} title={$t("tool.terminal")}>
              <span class="k">{$t("tool.terminal")}</span>
              <span class="v">{cmd}</span>
              <span class="tcc-caret" aria-hidden="true">{cmdExpanded ? "▾" : "▸"}</span>
            </button>
          {/if}
        {:else}
          {#each rows.slice(0, rowLimit) as row (row.key)}
            <div class="tcc-line"><span class="k">{row.key}</span><span class="v">{row.value}</span></div>
          {/each}
          {#if !isSearch && rows.length > MAX_PARAM_ROWS}
            <button class="tcc-more" onclick={() => paramsExpanded = !paramsExpanded}>
              {paramsExpanded
                ? `▾ ${$t("tool.collapse")}`
                : `▸ ${$t("tool.moreParams").replace("{n}", String(rows.length - MAX_PARAM_ROWS))}`}
            </button>
            {#if paramsExpanded}
              {#each rows.slice(MAX_PARAM_ROWS) as row (row.key)}
                <div class="tcc-line"><span class="k">{row.key}</span><span class="v">{row.value}</span></div>
              {/each}
            {/if}
          {/if}
        {/if}
        {#if shotUri}
          <img class="tcc-shot" src={shotUri} alt="computer screenshot preview" />
        {/if}
        {#if toolCall.result != null && !isTodo}
          <button
            class="tcc-res"
            class:err={hasError}
            class:expanded={resExpanded}
            onclick={() => resExpanded = !resExpanded}
            title={$t("tool.result")}
          >
            <span class="tcc-res-inner">
              {#if content != null}
                {paramLabel("content")}: {content}
              {:else if runRes}
                {runRes}
              {:else if searchRes}
                {searchRes}
              {:else if resRows}
                {#if resExpanded}
                  {#each resRows as r (r.key)}
                    <div class="tcc-rline"><span class="k">{r.key}</span><span class="v">{r.value}</span></div>
                  {/each}
                {:else}
                  {#each resRows.slice(0, MAX_PARAM_ROWS) as r (r.key)}
                    <div class="tcc-rline"><span class="k">{r.key}</span><span class="v">{r.value}</span></div>
                  {/each}
                  {#if resRows.length > MAX_PARAM_ROWS}
                    <div class="tcc-rmore">{$t("tool.moreParams").replace("{n}", String(resRows.length - MAX_PARAM_ROWS))}</div>
                  {/if}
                {/if}
              {:else}
                {shortResult(toolCall.result)}
              {/if}
            </span>
            <span class="tcc-caret" aria-hidden="true">{resExpanded ? "▾" : "▸"}</span>
          </button>
        {/if}
      </div>
    {/if}
  </div>
{/if}

<style>
  .tcc {
    /* 釉下暗纹: 折叠=一行青线, 展开=釉下显形 */
    margin: 2px 0;
    border-bottom: 1px solid var(--color-hairline);
    border-radius: 0;
    background: none;
  }
  .tcc.error .tcc-header {
    color: var(--color-error);
  }

  .tcc-header {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 7px 10px;
    background: none;
    border: none;
    color: var(--color-body);
    cursor: pointer;
    font-family: var(--font-serif);
    font-size: 12.5px;
    letter-spacing: 0.06em;
    text-align: left;
    transition: background 0.35s var(--ease-soak, ease);
    border-radius: 3px;
  }

  .tcc-header:hover {
    background: var(--color-primary-muted, rgba(147, 195, 214, 0.07));
  }

  .tcc-sym {
    flex-shrink: 0;
    font-size: 12px;
    line-height: 1;
    color: var(--color-primary);
    width: 14px;
    text-align: center;
  }

  .tcc-name {
    font-weight: 500;
    color: var(--color-ink);
    flex-shrink: 0;
  }

  .tcc-arg {
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
  .tcc-count {
    font-size: 10px;
    padding: 1px 5px;
    border-radius: 2px;
    background: var(--color-hairline);
    color: var(--text-tertiary);
    font-weight: 600;
    flex-shrink: 0;
  }

  .tcc-status {
    flex-shrink: 0;
    font-size: 10.5px;
    font-weight: 500;
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-family: var(--font-mono);
  }

  .tcc-status.done {
    color: var(--color-dim);
  }

  .tcc-status.running {
    color: var(--color-primary);
  }

  .tcc-status.err {
    color: var(--color-error);
  }

  .tcc-duration {
    flex-shrink: 0;
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--color-dim);
    font-weight: 500;
    font-variant-numeric: tabular-nums;
  }

  /* 展开: 釉下暗纹显形 (条件渲染, 折叠时零 DOM) */
  .tcc-body {
    margin: 4px 10px 10px;
    padding: 10px 14px;
    background: var(--color-primary-muted, rgba(147, 195, 214, 0.07));
    border: 1px solid var(--color-hairline);
    border-radius: 3px;
    box-shadow: var(--glaze-shadow, none);
    font-family: var(--font-mono);
    font-size: 11.5px;
    line-height: 1.7;
    color: var(--color-muted);
  }

  /* 第一行: path key + 值 (原型 k/v) */
  .tcc-line {
    display: flex;
    align-items: baseline;
    gap: 8px;
    white-space: nowrap;
    overflow: hidden;
  }
  .tcc-line .k {
    color: var(--color-primary);
    opacity: 0.7;
    flex: none;
  }
  .tcc-line .v {
    color: var(--color-ink);
    overflow: hidden;
    text-overflow: ellipsis;
  }

  /* 第二行: 终端命令 — 默认限高 3 行, 点击展开完整命令 */
  .tcc-line.tcc-cmd {
    margin-top: 4px;
    width: 100%;
    padding: 0;
    background: none;
    border: none;
    border-top: 1px dashed var(--color-hairline);
    text-align: left;
    cursor: pointer;
    color: inherit;
    font: inherit;
    align-items: flex-start;
    white-space: pre-wrap;
    word-break: break-all;
    overflow: hidden;
    max-height: 4.8em;
  }
  .tcc-line.tcc-cmd.expanded {
    max-height: none;
  }
  .tcc-line.tcc-cmd:hover .k {
    opacity: 1;
  }

  .tcc-caret {
    flex: none;
    align-self: center;
    color: var(--color-dim);
    font-size: 9px;
    opacity: 0.7;
  }

  /* 参数折叠按钮: 超出 3 行 → 点击展开其余 key */
  .tcc-more {
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
  .tcc-more:hover {
    opacity: 1;
  }

  /* 结果逐 key 行 (与参数行同形式) */
  .tcc-rline {
    display: flex;
    align-items: baseline;
    gap: 8px;
    white-space: nowrap;
    overflow: hidden;
  }
  .tcc-rline .k {
    color: var(--color-primary);
    opacity: 0.7;
    flex: none;
  }
  .tcc-rline .v {
    color: var(--color-ink);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .tcc-rmore {
    margin-top: 4px;
    color: var(--color-primary);
    font-size: 10.5px;
    opacity: 0.8;
  }

  /* computer_screenshot 内联预览 */
  .tcc-shot {
    display: block;
    width: 100%;
    margin-top: 6px;
    border-radius: 6px;
    border: 1px solid var(--color-hairline);
  }

  /* 第三行: 结果 — 默认限高滚动, 点击展开完整 */
  .tcc-res {
    margin-top: 4px;
    width: 100%;
    padding: 6px 0 0;
    background: none;
    border: none;
    border-top: 1px dashed var(--color-hairline);
    text-align: left;
    cursor: pointer;
    color: inherit;
    font: inherit;
    display: flex;
    align-items: flex-start;
    gap: 8px;
    white-space: pre-wrap;
    word-break: break-all;
    color: var(--color-ink);
    max-height: 100px;
    overflow-y: auto;
  }
  .tcc-res.expanded {
    max-height: none;
  }
  .tcc-res.err {
    color: var(--color-error);
  }
  .tcc-res-inner {
    flex: 1;
    min-width: 0;
  }
</style>
