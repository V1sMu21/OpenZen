<script lang="ts">
  import TodoDetailBubble from "./TodoDetailBubble.svelte";
  import { t } from "../i18n";

  let { items = [] as Array<{id:string;content:string;status:string;priority:string;order:number}> } = $props();

  let expanded = $state(false);
  let completed = $derived(items.filter(t => t.status === 'completed').length);
  let inProgress = $derived(items.filter(t => t.status === 'in_progress').length);
  let pending = $derived(items.filter(t => t.status === 'pending').length);
  let total = $derived(items.length);
  let hasItems = $derived(total > 0);

  function toggle() { expanded = !expanded; }

  function statusIcon(status: string): string {
    switch (status) {
      case 'completed': return '✅';
      case 'in_progress': return '◉';
      case 'cancelled': return '✕';
      default: return '⬜';
    }
  }
</script>

{#if hasItems}
  <div class="todo-progress">
    <button class="todo-toggle" onclick={toggle} type="button">
      <svg class="todo-chevron" class:expanded width="10" height="10" viewBox="0 0 10 10" fill="none">
        <path d="M3.5 2l3.5 3-3.5 3" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/>
      </svg>
      <span class="todo-label">
        ◉ {$t("todo.title")} {completed}/{total}
      </span>
      <span class="todo-detail">
        {completed} {$t("todo.completed")} · {inProgress} {$t("todo.inProgress")} · {pending} {$t("todo.pending")}
        {#if completed < total}
          <svg class="todo-arrow" width="10" height="10" viewBox="0 0 10 10" fill="none">
            <path d="M3 2l4 3-4 3" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
          </svg>
        {/if}
      </span>
    </button>

    {#if expanded}
      <TodoDetailBubble {items} onClose={toggle} />
    {/if}
  </div>
{/if}

<style>
  .todo-progress {
    margin: 4px 0;
    position: relative;
  }

  .todo-toggle {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 6px 10px;
    border: 1px dashed var(--color-hairline);
    border-radius: 6px;
    background: transparent;
    color: var(--color-dim);
    cursor: pointer;
    font-family: inherit;
    font-size: 12px;
    text-align: left;
    transition: color 0.15s, border-color 0.15s;
  }

  .todo-toggle:hover {
    color: var(--color-body);
    border-color: var(--color-hairline-strong);
  }

  .todo-chevron {
    flex: 0 0 auto;
    color: var(--color-muted);
    transition: transform 0.15s;
  }

  .todo-chevron.expanded {
    transform: rotate(90deg);
  }

  .todo-label {
    font-weight: 600;
    color: var(--color-accent);
  }

  .todo-detail {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    opacity: 0.7;
    white-space: nowrap;
    min-width: 0;
  }

  .todo-arrow {
    flex: 0 0 auto;
  }
</style>
