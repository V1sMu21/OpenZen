<script lang="ts">
  import type { Message } from "../stores/types";
  import { t } from "../i18n";

  let {
    messages = [] as Message[],
    currentIdx = 0,
    onNavigate = (idx: number) => {},
  } = $props();

  let branches = $derived.by(() => {
    const roots: Message[] = [];
    const seen = new Set<string>();
    for (const msg of messages) {
      if (!msg.children || msg.children.length < 2) continue;
      if (seen.has(msg.id)) continue;
      seen.add(msg.id);
      roots.push(msg);
    }
    return roots;
  });
</script>

{#if branches.length > 0}
  <div class="branch-nav">
    {#each branches as branch}
      {@const childIds = branch.children ?? []}
      {@const currentChild = messages.findIndex((m) => childIds.includes(m.id))}
      <div class="branch-group">
        <span class="branch-label">{$t("message.alternate")}</span>
        {#each childIds as childId, i}
          {@const childIdx = messages.findIndex((m) => m.id === childId)}
          <button
            class="branch-btn"
            class:active={childIdx === currentIdx || (currentIdx === -1 && i === childIds.length - 1)}
            onclick={() => childIdx >= 0 && onNavigate(childIdx)}
          >
            #{i + 1}
          </button>
        {/each}
      </div>
    {/each}
  </div>
{/if}

<style>
  .branch-nav {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    padding: 6px 16px;
    align-items: center;
  }
  .branch-group {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .branch-label {
    font-size: 11px;
    color: var(--color-dim);
    margin-right: 4px;
  }
  .branch-btn {
    background: var(--color-surface-elevated);
    border: 1px solid var(--color-hairline);
    color: var(--color-body);
    cursor: pointer;
    padding: 2px 10px;
    border-radius: 6px;
    font-size: 11px;
    transition: background 0.15s, border 0.15s;
  }
  .branch-btn:hover {
    border-color: var(--color-primary);
  }
  .branch-btn.active {
    background: var(--color-primary-muted);
    border-color: var(--color-primary);
    color: var(--color-primary);
  }
</style>
