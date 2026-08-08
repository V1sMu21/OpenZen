<script lang="ts">
  import { t, locale, tSync } from "../i18n";
  let lang = $state("zh");
  $effect(() => { lang = $locale; });
  interface Props {
    filePath: string;
    oldString: string;
    newString: string;
    collapsed?: boolean;
    durationMs?: number;
    result?: string;
    showTimer?: boolean;
  }

  let {
    filePath,
    oldString,
    newString,
    durationMs = undefined as number | undefined,
    result = undefined as any,
    completed = false,
    showTimer = false,
  } = $props();

  let collapsed = $state(true);

  function toggle() {
    collapsed = !collapsed;
  }

  function basename(path: string): string {
    const parts = path.split("/");
    return parts[parts.length - 1] || path;
  }

  function dirname(path: string): string {
    const idx = path.lastIndexOf("/");
    return idx >= 0 ? path.slice(0, idx) : "";
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

  function isError(): boolean {
    const r: any = result;
    if (r == null) return false;
    let text = "";
    try {
      if (typeof r === "string") {
        text = r;
        const parsed = JSON.parse(r);
        if (parsed && typeof parsed === "object") {
          return parsed.status === "error" || parsed.error != null;
        }
      } else if (typeof r === "object") {
        if (r.status === "error" || r.error != null) return true;
        text = JSON.stringify(r);
      } else {
        text = String(r);
      }
    } catch {
      text = typeof r === "string" ? r : String(r);
    }
    const lower = (text || "").toLowerCase();
    return lower.includes("error") || lower.includes("failed");
  }

  function truncateLine(line: string, max = 200): string {
    return line.length > max ? line.slice(0, max - 1) + "…" : line;
  }

  // Compute a simple unified diff between oldLines and newLines
  function computeDiff(oldLines: string[], newLines: string[]): Array<{
    oldNum: number | null;
    newNum: number | null;
    text: string;
    type: "removed" | "added" | "context";
  }> {
    const m = oldLines.length;
    const n = newLines.length;
    if (m === 0 && n === 0) return [];

    // LCS table (only keep last row for memory efficiency)
    // For long files we cap the diff to avoid O(m*n) blow-up
    const maxDiff = 2000;

    // Use a simplified patience-like diff: compare line-by-line
    // For small diffs (common case) do full LCS
    const useFullLcs = m * n <= maxDiff * 2;

    if (useFullLcs && m <= 500 && n <= 500) {
      return lcsDiff(oldLines, newLines);
    }

    // Fallback: show old removed, new added with line numbers
    return simpleDiff(oldLines, newLines);
  }

  function lcsDiff(oldLines: string[], newLines: string[]): Array<{
    oldNum: number | null;
    newNum: number | null;
    text: string;
    type: "removed" | "added" | "context";
  }> {
    const m = oldLines.length;
    const n = newLines.length;

    // Build LCS table
    const dp: number[][] = Array.from({ length: m + 1 }, () =>
      Array(n + 1).fill(0)
    );
    for (let i = 1; i <= m; i++) {
      for (let j = 1; j <= n; j++) {
        if (oldLines[i - 1] === newLines[j - 1]) {
          dp[i][j] = dp[i - 1][j - 1] + 1;
        } else {
          dp[i][j] = Math.max(dp[i - 1][j], dp[i][j - 1]);
        }
      }
    }

    // Backtrack to produce diff
    const result: Array<{
      oldNum: number | null;
      newNum: number | null;
      text: string;
      type: "removed" | "added" | "context";
    }> = [];

    let i = m;
    let j = n;
    while (i > 0 || j > 0) {
      if (i > 0 && j > 0 && oldLines[i - 1] === newLines[j - 1]) {
        result.unshift({
          oldNum: i,
          newNum: j,
          text: oldLines[i - 1],
          type: "context",
        });
        i--;
        j--;
      } else if (j > 0 && (i === 0 || dp[i][j - 1] >= dp[i - 1][j])) {
        result.unshift({
          oldNum: null,
          newNum: j,
          text: newLines[j - 1],
          type: "added",
        });
        j--;
      } else {
        result.unshift({
          oldNum: i,
          newNum: null,
          text: oldLines[i - 1],
          type: "removed",
        });
        i--;
      }
    }
    return result;
  }

  function simpleDiff(oldLines: string[], newLines: string[]): Array<{
    oldNum: number | null;
    newNum: number | null;
    text: string;
    type: "removed" | "added" | "context";
  }> {
    const result: Array<{
      oldNum: number | null;
      newNum: number | null;
      text: string;
      type: "removed" | "added" | "context";
    }> = [];
    for (let i = 0; i < oldLines.length; i++) {
      result.push({ oldNum: i + 1, newNum: null, text: oldLines[i], type: "removed" });
    }
    for (let i = 0; i < newLines.length; i++) {
      result.push({ oldNum: null, newNum: i + 1, text: newLines[i], type: "added" });
    }
    return result;
  }

  let dir = $derived(dirname(filePath));
  let name = $derived(basename(filePath));
  let hasError = $derived(isError());

  let diffLines = $derived.by(() => {
    const ol = oldString.length > 0 ? oldString.split("\n") : [];
    const nl = newString.length > 0 ? newString.split("\n") : [];
    return computeDiff(ol, nl);
  });

  let totalLines = $derived(diffLines.length);
</script>

<div class="edit-card">
  <button class="edit-header" onclick={toggle}>
    <svg class="edit-icon" width="13" height="13" viewBox="0 0 14 14" fill="none" aria-hidden="true">
      <path d="M9.5 1.5l3 3-7 7H2.5v-3l7-7z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/>
      <path d="M8 3l3 3" stroke="currentColor" stroke-width="1.2"/>
    </svg>
    <span class="edit-label">{$t("tool.edit")}</span>
    {#if dir}
      <span class="edit-dir">{dir}/</span>
    {/if}
    <span class="edit-name">{name}</span>
    <span class="edit-meta">·</span>
    <span class="edit-paste" title="Total changed lines">[Pasted ~{totalLines} lines]</span>
    {#if durationMs != null && showTimer}
      <span class="edit-duration">{formatDuration(durationMs)}</span>
    {/if}
    {#if result != null || completed}
      {#if hasError}
        <span class="edit-status err">{$t("tool.failed")}</span>
      {:else}
        <span class="edit-status done">已竟</span>
      {/if}
    {:else}
      <span class="edit-status running">{$t("tool.running")}</span>
    {/if}
  </button>
  {#if !collapsed}
    <div class="edit-body">
      <div class="diff-table">
        {#each diffLines as line, i}
          <div class="diff-line {line.type}">
            <span class="diff-old-num">{line.oldNum ?? ''}</span>
            <span class="diff-marker">
              {line.type === 'removed' ? '-' : line.type === 'added' ? '+' : ' '}
            </span>
            <span class="diff-new-num">{line.newNum ?? ''}</span>
            <span class="diff-text">{truncateLine(line.text)}</span>
          </div>
        {/each}
      </div>
      {#if result != null && hasError}
        <div class="edit-result err">{result}</div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .edit-card {
    margin: 6px 0;
    border: 1px solid var(--color-hairline, #1d1d1f);
    border-radius: 8px;
    background: var(--bg-secondary, #161618);
    overflow: hidden;
    font-size: 12px;
  }
  .edit-header {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 7px 10px;
    background: transparent;
    border: none;
    color: var(--text-secondary, #b8b0a3);
    cursor: pointer;
    text-align: left;
    font-family: inherit;
    font-size: 12px;
  }
  .edit-header:hover {
    background: rgba(255, 255, 255, 0.025);
  }
  .chevron {
    flex: 0 0 auto;
    color: var(--text-tertiary, #4d483e);
    transition: transform 0.15s;
  }
  .chevron.rotated {
    transform: rotate(90deg);
  }
  .edit-icon {
    flex: 0 0 auto;
    color: var(--text-tertiary, #4d483e);
  }
  .edit-label {
    font-weight: 500;
    color: var(--text-bright, #d6cfc0);
  }
  .edit-dir {
    color: var(--text-tertiary, #4d483e);
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
  }
  .edit-name {
    color: var(--text-bright, #d6cfc0);
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
    font-weight: 500;
  }
  .edit-meta {
    color: var(--text-tertiary, #4d483e);
  }
  .edit-paste {
    color: var(--text-tertiary, #4d483e);
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
    font-size: 11px;
  }
  .edit-duration {
    color: var(--text-tertiary, #4d483e);
    font-variant-numeric: tabular-nums;
    font-size: 11px;
  }
  .edit-status {
    font-size: 10px;
    padding: 1px 6px;
    border-radius: 4px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    margin-left: auto;
  }
  .edit-status.done {
    background: none;
    color: var(--text-tertiary, #4d483e);
  }
  .edit-status.running {
    background: rgba(245, 158, 11, 0.15);
    color: #f59e0b;
  }
  .edit-status.err {
    background: rgba(220, 90, 90, 0.15);
    color: #dc5a5a;
  }
  .edit-body {
    border-top: 1px solid var(--color-hairline, #1d1d1f);
    background: #0d0d0e;
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
    font-size: 11.5px;
    line-height: 1.55;
    max-height: 360px;
    overflow: auto;
  }
  .diff-table {
    display: block;
  }
  .diff-line {
    display: flex;
    align-items: flex-start;
    padding: 0 10px;
    white-space: pre;
  }
  .diff-old-num,
  .diff-new-num {
    flex: 0 0 32px;
    text-align: right;
    color: var(--text-tertiary, #4d483e);
    user-select: none;
    font-size: 11px;
    opacity: 0.6;
    line-height: 1.55;
    padding: 0 4px;
  }
  .diff-marker {
    flex: 0 0 10px;
    text-align: center;
    user-select: none;
    font-size: 12px;
    line-height: 1.55;
  }
  .diff-text {
    flex: 1;
    white-space: pre;
    color: var(--text-secondary, #b8b0a3);
    overflow-x: auto;
  }
  /* 行级 diff: GitHub 风格 — 删除行淡红底, 添加行淡绿底, 无词级高亮 */
  .diff-line.removed {
    background: rgba(220, 90, 90, 0.07);
  }
  .diff-line.removed .diff-marker {
    color: #dc5a5a;
  }
  .diff-line.removed .diff-text {
    color: #d6a8a8;
  }
  .diff-line.added {
    background: rgba(101, 184, 145, 0.07);
  }
  .diff-line.added .diff-marker {
    color: #65b891;
  }
  .diff-line.added .diff-text {
    color: #a8d6bc;
  }
  .diff-line.context .diff-old-num,
  .diff-line.context .diff-new-num {
    opacity: 0.35;
  }
  .diff-line.context {
    background: transparent;
  }
  .edit-result {
    padding: 6px 10px;
    border-top: 1px solid var(--color-hairline, #1d1d1f);
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
    font-size: 11px;
    color: var(--text-tertiary, #4d483e);
  }
  .edit-result.err {
    color: #dc5a5a;
  }
</style>
