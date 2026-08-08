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

  let isExpanded = $state(false);

  if (sessions.length > 0 && !isExpanded) {
    isExpanded = true;
  }
</script>

{#if sessions.length > 0}
  <div class="ungrouped-section">
    <div class="group-header" onclick={() => isExpanded = !isExpanded} role="treeitem" aria-expanded={isExpanded} tabindex="0">
      <span class="group-chevron">{isExpanded ? "▾" : "▸"}</span>
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
    margin: 4px 8px;
  }

  .group-header {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 12px;
    border-radius: 8px;
    cursor: pointer;
    user-select: none;
  }

  .group-header:hover {
    background: var(--color-surface-soft);
  }

  .group-chevron {
    font-size: 10px;
    color: var(--color-muted);
    width: 12px;
    text-align: center;
  }

  .group-label {
    flex: 1;
    font-size: 13px;
    font-weight: 600;
    color: var(--color-body);
  }

  .group-count {
    font-size: 11px;
    color: var(--color-muted);
    background: var(--color-surface-soft);
    padding: 1px 6px;
    border-radius: 10px;
  }

  .group-content {
    padding-left: 12px;
  }
</style>
