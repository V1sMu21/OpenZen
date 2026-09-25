<script lang="ts">
  import { t, locale, tSync } from "../i18n";
  import { computeDiff, diffStat as computeStat } from "../utils/diff";
  import { isFailedToolResult } from "../utils/fileChanges";
  import { basename, dirname, displayPath } from "../utils/paths";
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
    workingDir = "",
  } = $props();

  let collapsed = $state(true);

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

  function isError(): boolean {
    // Shared with the changed-files card: one verdict for "did this call
    // actually change the file", so the two can never disagree.
    return isFailedToolResult(result);
  }

  function truncateLine(line: string, max = 200): string {
    return line.length > max ? line.slice(0, max - 1) + "…" : line;
  }

  // 卡片头部显示相对路径 + 文件名 (悬停 title 可看完整绝对路径)
  let relPath = $derived(displayPath(filePath, workingDir));
  let dir = $derived(dirname(relPath));
  let name = $derived(basename(filePath));
  let hasError = $derived(isError());

  let diffLines = $derived.by(() => {
    const ol = oldString.length > 0 ? oldString.split("\n") : [];
    const nl = newString.length > 0 ? newString.split("\n") : [];
    return computeDiff(ol, nl);
  });

  let diffStat = $derived.by(() => computeStat(diffLines));
</script>

<div class="edit-card">
  <button class="edit-header" onclick={toggle} title={filePath}>
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
    <span
      class="edit-diffstat"
      title={$t("edit.diffstat")
        .replace("{add}", String(diffStat.added))
        .replace("{del}", String(diffStat.removed))}
      ><span class="diffstat-add">+{diffStat.added}</span><span class="diffstat-sep">,</span><span
        class="diffstat-del">-{diffStat.removed}</span
      ></span
    >
    {#if durationMs != null && showTimer}
      <span class="edit-duration">{formatDuration(durationMs)}</span>
    {/if}
    {#if result != null || completed}
      {#if hasError}
        <span class="edit-status err">{$t("tool.failed")}</span>
      {:else}
        <span class="edit-status done">{$t("status.done.short")}</span>
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
  .edit-diffstat {
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
    font-size: 11px;
    font-variant-numeric: tabular-nums;
  }
  .diffstat-add {
    color: var(--color-diff-add);
  }
  .diffstat-sep {
    color: var(--text-tertiary, #4d483e);
  }
  .diffstat-del {
    color: var(--color-diff-del);
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
