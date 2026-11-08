<script lang="ts">
  import { chat } from "../stores/chat";
  import TodoProgress from "./TodoProgress.svelte";
  import ReminderCard from "./ReminderCard.svelte";
  import { t } from "../i18n";

  // The old 320px right rail lived inside the message scroll area and kept
  // squeezing the bubbles once the preview panel was open. Tasks are now a
  // pair of chips above the composer; details open in a floating popover.
  let open = $state(false);

  const todos = $derived($chat.todos);
  const reminders = $derived($chat.reminders);
  const doneTodos = $derived(todos.filter((x) => x.status === "completed").length);
  const activeReminders = $derived(reminders.filter((r) => r.status === "active").length);

  function closeOnEscape(e: KeyboardEvent) {
    if (e.key === "Escape") open = false;
  }
</script>

<svelte:window onkeydown={closeOnEscape} />

{#if todos.length > 0 || reminders.length > 0}
  <div class="task-chips">
    {#if todos.length > 0}
      <button
        type="button"
        class="task-chip"
        class:on={open}
        aria-expanded={open}
        onclick={() => (open = !open)}
      >
        <span class="chip-mark">✓</span>
        {$t("tasks.todoChip").replace("{done}", String(doneTodos)).replace("{total}", String(todos.length))}
      </button>
    {/if}
    {#if reminders.length > 0}
      <button
        type="button"
        class="task-chip reminder"
        class:on={open}
        aria-expanded={open}
        onclick={() => (open = !open)}
      >
        <span class="chip-mark">◷</span>
        {$t("tasks.reminderChip").replace("{active}", String(activeReminders)).replace("{total}", String(reminders.length))}
      </button>
    {/if}

    {#if open}
      <button class="task-backdrop" aria-label={$t("tasks.close")} onclick={() => (open = false)}></button>
      <div class="task-pop" role="dialog" aria-label={$t("tasks.title")}>
        {#if todos.length > 0}
          <TodoProgress items={todos} />
        {/if}
        {#if reminders.length > 0}
          <ReminderCard items={reminders} />
        {/if}
      </div>
    {/if}
  </div>
{/if}

<style>
  .task-chips {
    position: relative;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 24px 0;
  }

  .task-chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 24px;
    padding: 0 9px;
    border-radius: 7px;
    border: 1px solid var(--color-hairline);
    background: var(--color-surface-soft);
    color: var(--color-body);
    font-size: 10.5px;
    cursor: pointer;
    white-space: nowrap;
  }
  .task-chip:hover { border-color: var(--color-hairline-strong); color: var(--color-ink); }
  .task-chip.on { border-color: var(--color-primary); color: var(--color-ink); }
  .chip-mark { color: var(--color-primary); font-size: 10px; }

  .task-backdrop {
    position: fixed;
    inset: 0;
    z-index: 70;
    background: transparent;
    border: none;
    padding: 0;
  }

  .task-pop {
    position: absolute;
    left: 24px;
    bottom: calc(100% + 8px);
    z-index: 71;
    width: min(420px, calc(100vw - 48px));
    max-height: 56vh;
    overflow-y: auto;
    padding: 10px;
    background: var(--color-surface-overlay);
    border: 1px solid var(--color-hairline-strong);
    border-radius: 12px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .task-pop :global(.todo-progress),
  .task-pop :global(.reminder-card) {
    margin: 0;
    background: transparent;
    border: none;
    width: 100%;
  }
</style>
