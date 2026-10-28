<script lang="ts">
  import SessionList from "./SessionList.svelte";
  import type { SessionInfo } from "../api/sessions";
  import { t } from "../i18n";

  let {
    sessions = $bindable([] as SessionInfo[]),
    currentSessionId = $bindable<string | null>(null),
    onSelectSession = $bindable<(id: string) => void>(() => {}),
    onEscape = $bindable<(() => void) | null>(null),
  } = $props();

  // 默认展开: 侧栏载入时 sessions 可能还没到, 旧逻辑只在这里判一次,
  // 结果首屏永远是折叠的 —— 看起来像“没有会话”。用户可以手动折叠。
  let isExpanded = $state(true);
</script>

{#if sessions.length > 0}
  <div class="ungrouped-section">
    <div class="group-header" onclick={() => isExpanded = !isExpanded} role="treeitem" aria-expanded={isExpanded} tabindex="0">
      <svg class="group-chevron" width="9" height="9" viewBox="0 0 10 10" fill="none" aria-hidden="true">
        <path d={isExpanded ? "M1.5 3.5L5 7l3.5-3.5" : "M3.5 1.5L7 5l-3.5 3.5"} stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/>
      </svg>
      <span class="group-label">{$t("project.ungrouped")}</span>
      <span class="group-count">{sessions.length}</span>
    </div>
    {#if isExpanded}
      <div class="group-content">
        <SessionList
          bind:items={sessions}
          bind:currentId={currentSessionId}
          {onSelectSession}
          {onEscape}
          indentLevel={1}
        />
      </div>
    {/if}
  </div>
{/if}

<style>
  .ungrouped-section {
    margin: 0;
  }

  .group-header {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 6px 10px;
    border-radius: 9px;
    cursor: pointer;
    user-select: none;
    transition: background 0.15s;
  }

  .group-header:hover {
    background: var(--color-surface-soft);
  }

  .group-chevron {
    flex: none;
    color: var(--color-muted);
  }

  .group-label {
    flex: 1;
    font-size: 12px;
    font-weight: 500;
    color: var(--color-body);
    letter-spacing: 0.01em;
  }

  .group-count {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--color-muted);
    background: var(--color-surface-soft);
    padding: 1px 6px;
    border-radius: 999px;
    font-variant-numeric: tabular-nums;
  }

  .group-content {
    padding-left: 4px;
  }
</style>
