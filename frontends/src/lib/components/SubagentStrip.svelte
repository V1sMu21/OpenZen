<script lang="ts">
  import { onMount } from "svelte";
  import { subagents } from "../stores/subagents.svelte";
  import { sidepanel } from "../stores/sidepanel.svelte";
  import { t } from "../i18n";
  import { isTauri } from "../api/tauri";

  onMount(() => void subagents.init());

  // Same aggregation rule as background jobs: several children collapse into
  // "N 个子代理" and expand on click.
  let expanded = $state(false);
  const shown = $derived(
    subagents.items.filter((s) => s.status === "running" || s.needs_input),
  );
  const collapsed = $derived(shown.length > 1 && !expanded);
  const anyRunning = $derived(shown.some((s) => s.status === "running"));

  function openPanel() {
    if (!isTauri()) return;
    void sidepanel.open({
      type: "background",
      path: "background://panel",
      label: $t("jobs.panel"),
    });
  }

  function openTranscript(id: string) {
    openPanel();
    // The panel lists children; the clicked one is selected there.
    window.dispatchEvent(new CustomEvent("oz:subagent-focus", { detail: id }));
  }
</script>

{#snippet agentRow(s: (typeof shown)[number])}
  <button
    class="ss-chip"
    class:needs={s.needs_input}
    class:running={s.status === "running"}
    onclick={() => openTranscript(s.id)}
    title={s.label}
  >
    <i class="ss-dot"></i>
    <span class="ss-label">{s.label}</span>
    <span class="ss-state">
      {s.needs_input ? $t("subagent.needsInput") : $t("subagent.running")}
    </span>
  </button>
{/snippet}

{#if shown.length > 0}
  <div class="sub-strip" role="status">
    {#if shown.length > 1}
      <button
        class="ss-summary"
        class:live={anyRunning}
        aria-expanded={expanded}
        title={expanded ? $t("subagent.collapse") : $t("subagent.expand")}
        onclick={() => (expanded = !expanded)}
      >
        <i class="ss-dot" class:run={anyRunning}></i>
        {$t("subagent.count").replace("{n}", String(shown.length))}
        <span class="ss-chevron">{expanded ? "▴" : "▾"}</span>
      </button>
    {:else}
      <span class="ss-title">{$t("subagent.title")}</span>
    {/if}
    {#if expanded}
      <div class="ss-list">
        {#each shown as s (s.id)}
          {@render agentRow(s)}
        {/each}
      </div>
    {:else if shown.length === 1}
      {@render agentRow(shown[0])}
    {/if}
  </div>
{/if}

<style>
  .sub-strip {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 8px;
    padding: 8px 24px 0;
    overflow-x: auto;
    scrollbar-width: none;
  }
  .sub-strip::-webkit-scrollbar { display: none; }

  .ss-summary {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    flex: none;
    height: 26px;
    padding: 0 10px;
    border-radius: 8px;
    border: 1px solid var(--color-hairline);
    background: var(--color-surface-soft);
    color: var(--color-body);
    font-size: 10.5px;
    cursor: pointer;
  }
  .ss-summary:hover { border-color: var(--color-hairline-strong); color: var(--color-ink); }
  .ss-summary.live { border-color: var(--color-hairline-strong); }
  .ss-chevron { font-size: 9px; color: var(--color-muted); }
  .ss-title {
    flex: none;
    font-size: 9.5px;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--color-muted);
  }

  .ss-list {
    display: flex;
    flex-direction: column;
    gap: 4px;
    width: 100%;
    max-width: 620px;
    max-height: 168px;
    overflow-y: auto;
  }
  .ss-list .ss-chip { width: 100%; max-width: none; }
  .ss-list .ss-label { flex: 1; max-width: none; }

  .ss-chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    flex: none;
    height: 26px;
    padding: 0 9px;
    border-radius: 8px;
    border: 1px solid var(--color-hairline);
    background: var(--color-surface-soft);
    color: var(--color-body);
    font-size: 10.5px;
    cursor: pointer;
    max-width: 300px;
  }
  .ss-chip:hover { border-color: var(--color-hairline-strong); color: var(--color-ink); }
  .ss-chip.needs { border-color: var(--color-gold-line); color: var(--color-gold); }

  .ss-dot {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--color-muted);
    flex: none;
  }
  .ss-chip.running .ss-dot {
    background: var(--color-primary);
    animation: ss-breathe 1.8s ease-in-out infinite;
  }
  .ss-chip.needs .ss-dot { background: var(--color-gold); }
  @keyframes ss-breathe { 50% { opacity: 0.25; } }

  .ss-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 180px;
  }
  .ss-state { font-size: 9.5px; color: var(--color-muted); }
  .ss-chip.needs .ss-state { color: var(--color-gold); }
</style>
