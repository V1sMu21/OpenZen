<script lang="ts">
  import { onMount } from "svelte";
  import { jobs } from "../stores/jobs.svelte";
  import { t } from "../i18n";

  let now = $state(Date.now());
  onMount(() => {
    void jobs.init();
    const timer = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(timer);
  });

  function elapsed(job: { started_at: number; finished_at?: number | null }): string {
    const end = job.finished_at ? job.finished_at * 1000 : now;
    const secs = Math.max(0, Math.round((end - job.started_at * 1000) / 1000));
    if (secs < 60) return `${secs}s`;
    const m = Math.floor(secs / 60);
    return `${m}m${String(secs % 60).padStart(2, "0")}s`;
  }

  // Multiple jobs collapse into one summary chip ("5 个后台任务"); the list
  // only appears after the user clicks it. A single job keeps its chip so
  // the common case needs no extra click.
  let expanded = $state(false);
  const shown = $derived(jobs.items);
  const collapsed = $derived(shown.length > 1 && !expanded);
  const runningCount = $derived(shown.filter((j) => j.status === "running" || j.status === "stopping").length);
</script>

{#if shown.length > 0}
  <div class="job-strip" role="status" aria-label={$t("jobs.background")}>
    {#if collapsed}
      <button
        class="js-summary"
        class:live={runningCount > 0}
        title={$t("jobs.expand")}
        onclick={() => (expanded = true)}
      >
        <i class="js-dot" class:run={runningCount > 0}></i>
        {$t("jobs.count").replace("{n}", String(shown.length))}
        <span class="js-chevron">▾</span>
      </button>
    {:else}
    <span class="js-title">{$t("jobs.background")}</span>
    {#each shown as job (job.id)}
      <span class="js-chip" class:done={job.status !== "running" && job.status !== "stopping"}>
        <i class="js-dot" class:run={job.status === "running" || job.status === "stopping"} class:bad={job.status === "failed"}></i>
        <span class="js-id">{job.id}</span>
        <span class="js-label" title={job.label}>{job.label}</span>
        <span class="js-time">{elapsed(job)}</span>
        {#if job.status === "running" || job.status === "stopping"}
          <button class="js-kill" onclick={() => jobs.kill(job.id)} title={$t("jobs.stop")} aria-label={$t("jobs.stop")}>✕</button>
        {:else}
          <span class="js-state">{job.status === "completed" ? "✓" : job.status === "killed" ? "■" : "!"}</span>
        {/if}
      </span>
    {/each}
    {#if shown.length > 1}
      <button class="js-collapse" title={$t("jobs.collapse")} onclick={() => (expanded = false)}>▴</button>
    {/if}
    {/if}
  </div>
{/if}

<style>
  .job-strip {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 24px 0;
    overflow-x: auto;
    scrollbar-width: none;
  }
  .job-strip::-webkit-scrollbar { display: none; }

  .js-summary {
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
  .js-summary:hover { border-color: var(--color-hairline-strong); color: var(--color-ink); }
  .js-summary.live { border-color: var(--color-hairline-strong); }
  .js-chevron { font-size: 9px; color: var(--color-muted); }
  .js-collapse {
    flex: none;
    width: 22px;
    height: 22px;
    border: none;
    background: none;
    color: var(--color-muted);
    cursor: pointer;
    font-size: 10px;
  }
  .js-collapse:hover { color: var(--color-ink); }

  .js-title {
    flex: none;
    font-size: 9.5px;
    letter-spacing: 0.14em;
    color: var(--color-muted);
    text-transform: uppercase;
  }

  .js-chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    flex: none;
    height: 26px;
    padding: 0 8px;
    border-radius: 8px;
    border: 1px solid var(--color-hairline);
    background: var(--color-surface-soft);
    font-size: 10.5px;
    color: var(--color-dim);
    max-width: 320px;
  }
  .js-chip.done { opacity: 0.65; }

  .js-dot {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--color-muted);
    flex: none;
  }
  .js-dot.run { background: var(--color-primary); animation: js-breathe 1.8s ease-in-out infinite; }
  .js-dot.bad { background: var(--color-error); }
  @keyframes js-breathe { 50% { opacity: 0.25; } }

  .js-id {
    font-family: var(--font-mono);
    font-size: 9.5px;
    color: var(--color-muted);
  }
  .js-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 150px;
    color: var(--color-body);
  }
  .js-time {
    font-family: var(--font-mono);
    font-size: 9.5px;
    color: var(--color-muted);
    font-variant-numeric: tabular-nums;
  }
  .js-kill {
    border: none;
    background: none;
    color: var(--color-muted);
    cursor: pointer;
    width: 18px;
    height: 18px;
    border-radius: 5px;
    line-height: 1;
  }
  .js-kill:hover { background: var(--color-surface-overlay); color: var(--color-error); }
  .js-state { color: var(--color-muted); }
</style>
