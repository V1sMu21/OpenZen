<script lang="ts">
  import type { ReminderTask } from "../stores/chat";
  import { t } from "../i18n";

  let { items = [] as ReminderTask[] } = $props();

  let expanded = $state(false);
  let active = $derived(items.filter((r) => r.status === "active").length);
  let total = $derived(items.length);
  let hasItems = $derived(total > 0);

  function toggle() { expanded = !expanded; }

  /** e.g. "3s", "12s", "5m", "1h" — compact duration for card rows */
  function fmtDuration(secs: number): string {
    if (secs < 60) return `${secs}s`;
    if (secs < 3600) return `${Math.round(secs / 60)}m`;
    return `${(secs / 3600).toFixed(1)}h`;
  }

  function fmtFireTime(ms: number): string {
    const d = new Date(ms);
    const hh = String(d.getHours()).padStart(2, "0");
    const mm = String(d.getMinutes()).padStart(2, "0");
    return `${hh}:${mm}`;
  }

  function truncate(s: string, max = 40): string {
    return s.length > max ? s.slice(0, max - 1) + "…" : s;
  }
</script>

{#if hasItems}
  <div class="reminder-card">
    <button class="reminder-toggle" onclick={toggle} type="button">
      <svg class="reminder-chevron" class:expanded width="10" height="10" viewBox="0 0 10 10" fill="none">
        <path d="M3.5 2l3.5 3-3.5 3" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/>
      </svg>
      <span class="reminder-label">
        ⏰ {$t("reminder.title")} {active}/{total}
      </span>
      <span class="reminder-detail">
        {#if active > 0}
          {$t("reminder.active")} {active}
        {:else}
          {$t("reminder.allDone")}
        {/if}
      </span>
    </button>

    {#if expanded}
      <div class="reminder-list">
        {#each items as r (r.id)}
          <div class="reminder-item" class:done={r.status === "done"}>
            <span class="reminder-status" title={r.status === "done" ? $t("reminder.done") : $t("reminder.active")}>
              {r.status === "done" ? "✅" : (r.repeatCount > 0 ? "🔁" : "⏰")}
            </span>
            <div class="reminder-body">
              <span class="reminder-title" title={r.title}>{truncate(r.title)}</span>
              <span class="reminder-meta">
                {#if r.repeatCount > 0}
                  {$t("reminder.heartbeat")} · {$t("reminder.every")} {fmtDuration(r.repeatIntervalSecs)} · {$t("reminder.next")} {fmtFireTime(r.fireAtMs)}
                  {#if r.status === "active"}· {r.remaining}/{$t("reminder.left")}{/if}
                {:else}
                  {$t("reminder.once")} · {$t("reminder.next")} {fmtFireTime(r.fireAtMs)}
                {/if}
              </span>
            </div>
          </div>
        {/each}
      </div>
    {/if}
  </div>
{/if}

<style>
  .reminder-card {
    margin: 4px 0;
    position: relative;
  }

  .reminder-toggle {
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

  .reminder-toggle:hover {
    color: var(--color-body);
    border-color: var(--color-hairline-strong);
  }

  .reminder-chevron {
    flex: 0 0 auto;
    color: var(--color-muted);
    transition: transform 0.15s;
  }

  .reminder-chevron.expanded {
    transform: rotate(90deg);
  }

  .reminder-label {
    font-weight: 600;
    color: var(--color-accent);
  }

  .reminder-detail {
    margin-left: auto;
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    opacity: 0.7;
    white-space: nowrap;
  }

  .reminder-list {
    margin-top: 4px;
    border: 1px solid var(--color-hairline);
    border-radius: 6px;
    background: var(--color-surface-soft);
    padding: 4px;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .reminder-item {
    display: flex;
    align-items: flex-start;
    gap: 6px;
    padding: 5px 8px;
    border-radius: 4px;
    font-size: 12px;
  }

  .reminder-item:hover {
    background: var(--color-surface-elevated);
  }

  .reminder-item.done {
    opacity: 0.45;
  }

  .reminder-status {
    flex: 0 0 auto;
    font-size: 12px;
    line-height: 1.4;
  }

  .reminder-body {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  .reminder-title {
    color: var(--color-body);
    font-size: 12px;
    line-height: 1.4;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 220px;
  }

  .reminder-meta {
    font-size: 10.5px;
    color: var(--color-muted);
    font-variant-numeric: tabular-nums;
  }
</style>
