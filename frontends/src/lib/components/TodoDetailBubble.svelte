<script lang="ts">
  import { t } from "../i18n";
  let { items = [] as Array<{id:string;content:string;status:string;priority:string;order:number}>, onClose = $bindable<(() => void) | undefined>(undefined) } = $props();

  function handleClose() { onClose?.(); }

  function statusIcon(status: string): string {
    switch (status) {
      case 'completed': return '✓';
      case 'in_progress': return '●';
      case 'cancelled': return '✕';
      default: return '○';
    }
  }

  function statusClass(status: string): string {
    return `status-${status}`;
  }
</script>

<div class="todo-bubble" role="dialog">
  <div class="todo-bubble-header">
    <span class="todo-bubble-title">◉ {$t("todo.detail.title")} ({items.length})</span>
    <button class="todo-bubble-close" onclick={handleClose} aria-label={$t("shortcuts.dialogClose")} type="button">
      <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
        <path d="M2 2l8 8M10 2l-8 8" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
      </svg>
    </button>
  </div>

  <div class="todo-bubble-list">
    {#each items as item (item.id)}
      <div class="todo-item {statusClass(item.status)}">
        <span class="todo-item-icon">{statusIcon(item.status)}</span>
        <span class="todo-item-content" class:strikethrough={item.status === 'cancelled'}>
          {item.content}
        </span>
        {#if item.priority === 'high'}
          <span class="todo-item-prio high">high</span>
        {/if}
      </div>
    {/each}
  </div>
</div>

<style>
  .todo-bubble {
    position: absolute;
    left: 0;
    right: 0;
    top: 100%;
    margin-top: 4px;
    width: 100%;
    min-width: 0;
    max-width: none;
    background: var(--color-surface-overlay);
    border: 1px solid var(--color-hairline-strong);
    border-radius: 8px;
    padding: 8px;
    z-index: 10;
    box-shadow: 0 4px 12px rgba(0,0,0,0.3);
  }

  .todo-bubble-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 4px 6px;
    margin-bottom: 4px;
    border-bottom: 1px solid var(--color-hairline);
  }

  .todo-bubble-title {
    font-size: 12px;
    font-weight: 600;
    color: var(--color-accent);
  }

  .todo-bubble-close {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    border-radius: 4px;
    background: none;
    border: none;
    color: var(--color-dim);
    cursor: pointer;
  }

  .todo-bubble-close:hover {
    background: var(--color-surface-soft);
    color: var(--color-ink);
  }

  .todo-bubble-list {
    display: flex;
    flex-direction: column;
    gap: 2px;
    max-height: 300px;
    overflow-y: auto;
  }

  .todo-item {
    display: flex;
    align-items: flex-start;
    gap: 6px;
    padding: 4px 6px;
    border-radius: 4px;
    font-size: 12px;
    color: var(--color-body);
  }

  .todo-item.status-completed {
    opacity: 0.7;
    background: rgba(93, 184, 114, 0.06);
  }

  .todo-item.status-in_progress {
    background: rgba(212, 160, 23, 0.08);
  }

  .todo-item-icon {
    flex-shrink: 0;
    font-size: 11px;
    width: 14px;
    text-align: center;
  }

  .todo-item.status-completed .todo-item-icon { color: #5db872; }
  .todo-item.status-in_progress .todo-item-icon { color: #d4a017; }
  .todo-item.status-cancelled .todo-item-icon { color: var(--color-error, #c64545); }

  .todo-item-content {
    flex: 1;
    line-height: 1.4;
    overflow-wrap: break-word;
  }

  .todo-item-content.strikethrough {
    text-decoration: line-through;
    opacity: 0.6;
  }

  .todo-item-prio {
    flex-shrink: 0;
    font-size: 9px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.4px;
    padding: 1px 4px;
    border-radius: 3px;
    background: rgba(204, 120, 92, 0.15);
    color: var(--color-primary);
  }

  @media (max-width: 1100px) {
    .todo-bubble {
      position: static;
      margin-top: 4px;
      max-width: none;
      box-shadow: none;
      border: 1px solid var(--color-hairline);
    }
  }
</style>
