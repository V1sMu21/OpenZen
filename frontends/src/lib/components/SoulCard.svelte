<script lang="ts">
  /**
   * SoulCard (P2-19/P2-20) — the "companion presence" surface.
   *
   * Everything visible here comes from real ERME/session state, not
   * placeholders: identity/mood/portrait/memory counts from the soul
   * store, the unfinished todo count from the live chat state, and the
   * durable-reminder count from the reminder rail. Self-hides entirely
   * when the memory backend is off.
   *
   * On a "return" (first open of the day, or >6h since the last ack) it
   * adds one welcome-back line built from those same facts — the minimal
   * proactive presence the vision doc asks for.
   */
  import { onMount } from "svelte";
  import { soulStore } from "../stores/soul.svelte";
  import { soulDisplayName } from "../api/settings";
  import { chat } from "../stores/chat";
  import { t } from "../i18n";

  const ACK_KEY = "openzen:soulcard:ack";
  const RETURN_AFTER_MS = 6 * 60 * 60 * 1000;

  let dismissed = $state(false);
  let showWelcome = $state(false);

  onMount(() => {
    void soulStore.load();
    const last = Number(localStorage.getItem(ACK_KEY) ?? 0);
    showWelcome = Date.now() - last > RETURN_AFTER_MS;
  });

  let status = $derived(soulStore.status);
  let enabled = $derived(!!status?.enabled);
  let name = $derived(soulDisplayName(status) ?? $t("soul.newborn", "记忆体"));
  let openTodos = $derived(
    ($chat.todos ?? []).filter((todo) => todo.status !== "completed").length,
  );
  let activeReminders = $derived(
    ($chat.reminders ?? []).filter((r) => r.status === "active").length,
  );

  function dismiss() {
    dismissed = true;
    localStorage.setItem(ACK_KEY, String(Date.now()));
  }
</script>

{#if enabled && !dismissed}
  <div class="soul-card" role="status">
    <div class="soul-line">
      <span class="soul-name">{name}</span>
      {#if status?.soul?.mood}<span class="soul-mood">{status.soul.mood}</span>{/if}
      <span class="soul-stat">
        {$t("soul.memories")} {status?.store?.total_entries ?? 0}
      </span>
      <span class="soul-stat">
        {$t("soul.portraitFacts")} {status?.soul?.portrait_facts ?? 0}
      </span>
      {#if openTodos > 0}
        <span class="soul-stat">· {openTodos} {$t("status.todos", "todos")}</span>
      {/if}
      {#if activeReminders > 0}
        <span class="soul-stat">· {activeReminders} {$t("reminder.title", "reminders")}</span>
      {/if}
      <button class="soul-dismiss" onclick={dismiss} aria-label={$t("settings.close")}>✕</button>
    </div>
    {#if showWelcome}
      <div class="soul-welcome">
        {$t("soul.welcomeBack", "Welcome back")}{#if openTodos > 0} — {openTodos}
          {$t("soul.pendingWork", "open item(s) from last time")}{/if}{#if status?.soul?.narrative_chapters}
          · {$t("soul.narrative")} {status.soul.narrative_chapters} {$t("soul.chapters")}{/if}
      </div>
    {/if}
  </div>
{/if}

<style>
  .soul-card {
    margin: 6px 16px 0;
    padding: 6px 10px;
    border: 1px solid var(--color-hairline);
    border-radius: 8px;
    background: var(--color-surface-soft);
    font-size: 12px;
    color: var(--color-muted);
  }
  .soul-line {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .soul-name {
    color: var(--color-body);
    font-family: var(--font-serif);
  }
  .soul-mood {
    opacity: 0.8;
  }
  .soul-stat {
    opacity: 0.85;
  }
  .soul-dismiss {
    margin-left: auto;
    border: none;
    background: transparent;
    color: var(--color-dim);
    cursor: pointer;
    padding: 0 4px;
  }
  .soul-welcome {
    margin-top: 4px;
    color: var(--color-primary);
  }
</style>
