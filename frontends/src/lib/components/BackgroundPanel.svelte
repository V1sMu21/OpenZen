<script lang="ts">
  import { onMount } from "svelte";
  import { jobs } from "../stores/jobs.svelte";
  import { subagents } from "../stores/subagents.svelte";
  import { isTauri, tauriInvoke } from "../api/tauri";
  import { t } from "../i18n";

  let { artifact }: { artifact: { path: string } } = $props();

  let tab = $state<"jobs" | "subagents">("jobs");
  let expandedJob = $state<string | null>(null);
  let jobText = $state("");
  let jobError = $state("");
  let focusChild = $state<string | null>(null);
  let transcript = $state("");
  let transcriptLoading = $state(false);

  onMount(() => {
    void jobs.init();
    void subagents.init();
    const onFocus = (e: Event) => {
      const id = (e as CustomEvent<string>).detail;
      tab = "subagents";
      void loadTranscript(id);
    };
    window.addEventListener("oz:subagent-focus", onFocus);
    return () => window.removeEventListener("oz:subagent-focus", onFocus);
  });

  async function readJob(id: string) {
    if (expandedJob === id) {
      expandedJob = null;
      return;
    }
    expandedJob = id;
    jobText = "";
    jobError = "";
    if (!isTauri()) return;
    try {
      const res = (await tauriInvoke("job_output", {
        sessionId: jobs.backgroundItems.find((j) => j.id === id)?.owner_session ?? "",
        jobId: id,
      })) as { text?: string };
      jobText = res?.text || "";
      if (!jobText) jobText = $t("jobs.noOutput");
    } catch (e) {
      jobError = String(e);
    }
  }

  async function cancelSubagent(id: string) {
    if (!isTauri()) return;
    try {
      await tauriInvoke("agent_cancel", { childId: id });
    } catch {
      /* it may have just finished */
    }
  }

  async function loadTranscript(id: string) {
    focusChild = id;
    transcript = "";
    transcriptLoading = true;
    try {
      transcript = await subagents.transcript(id);
    } finally {
      transcriptLoading = false;
    }
  }
</script>

<div class="bp">
  <div class="bp-tabs">
    <button class="bp-tab" class:on={tab === "jobs"} onclick={() => (tab = "jobs")}>
      {$t("jobs.background")} <span class="bp-count">{jobs.backgroundItems.length}</span>
    </button>
    <button class="bp-tab" class:on={tab === "subagents"} onclick={() => (tab = "subagents")}>
      {$t("subagent.title")} <span class="bp-count">{subagents.items.length}</span>
    </button>
  </div>

  {#if tab === "jobs"}
    {#if jobs.backgroundItems.length === 0}
      <p class="bp-empty">{$t("jobs.empty")}</p>
    {:else}
      <ul class="bp-list">
        {#each jobs.backgroundItems as job (job.id)}
          <li class="bp-row" class:active={expandedJob === job.id}>
            <button class="bp-row-main" onclick={() => readJob(job.id)}>
              <span class="bp-state {job.status}"></span>
              <span class="bp-id">{job.id}</span>
              <span class="bp-label" title={job.label}>{job.label}</span>
              <span class="bp-status">{job.status}</span>
            </button>
            {#if job.status === "running" || job.status === "stopping"}
              <button class="bp-kill" onclick={() => jobs.kill(job.id)} title={$t("jobs.stop")}>✕</button>
            {/if}
            {#if expandedJob === job.id}
              <pre class="bp-output">{jobError || jobText}</pre>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  {:else}
    {#if subagents.items.length === 0}
      <p class="bp-empty">{$t("subagent.empty")}</p>
    {:else}
      <ul class="bp-list">
        {#each subagents.items as s (s.id)}
          <li class="bp-row" class:active={focusChild === s.id}>
            <button class="bp-row-main" onclick={() => loadTranscript(s.id)}>
              <span class="bp-state {s.status}"></span>
              <span class="bp-label" title={s.label}>{s.label}</span>
              {#if s.needs_input}<span class="bp-needs">{$t("subagent.needsInput")}</span>{/if}
              <span class="bp-status">{s.status}</span>
            </button>
            {#if s.status === "running"}
              <button class="bp-kill" onclick={() => cancelSubagent(s.id)} title={$t("jobs.stop")}>✕</button>
            {/if}
          </li>
        {/each}
      </ul>
      {#if transcriptLoading}
        <p class="bp-empty">{$t("memory.loading")}</p>
      {:else if transcript}
        <pre class="bp-transcript">{transcript}</pre>
      {/if}
    {/if}
  {/if}
</div>

<style>
  .bp {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    padding: 12px;
    gap: 10px;
  }

  .bp-tabs { display: flex; gap: 4px; flex: none; }
  .bp-tab {
    height: 28px;
    padding: 0 10px;
    border-radius: 8px;
    border: 1px solid transparent;
    background: transparent;
    color: var(--color-muted);
    font-size: 11.5px;
    cursor: pointer;
  }
  .bp-tab.on {
    background: var(--color-surface-soft);
    border-color: var(--color-hairline);
    color: var(--color-ink);
  }
  .bp-count {
    font-family: var(--font-mono);
    font-size: 9px;
    opacity: 0.7;
    margin-left: 4px;
  }

  .bp-list {
    list-style: none;
    margin: 0;
    padding: 0;
    overflow-y: auto;
    flex: 1;
    min-height: 0;
  }
  .bp-row {
    border: 1px solid var(--color-hairline);
    border-radius: 10px;
    margin-bottom: 6px;
    background: var(--color-surface-soft);
    overflow: hidden;
  }
  .bp-row-main {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    height: 32px;
    padding: 0 10px;
    border: none;
    background: none;
    color: var(--color-body);
    font-size: 11.5px;
    cursor: pointer;
    text-align: left;
  }
  .bp-state {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--color-muted);
    flex: none;
  }
  .bp-state.running { background: var(--color-primary); }
  .bp-state.failed, .bp-state.interrupted { background: var(--color-error); }
  .bp-state.completed { background: var(--color-success); }
  .bp-id { font-family: var(--font-mono); font-size: 9.5px; color: var(--color-muted); }
  .bp-label { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .bp-status { font-size: 9.5px; color: var(--color-muted); }
  .bp-needs { font-size: 9.5px; color: var(--color-gold); }
  .bp-kill {
    position: absolute;
    right: 8px;
    border: none;
    background: none;
    color: var(--color-muted);
    cursor: pointer;
  }
  .bp-row { position: relative; }
  .bp-kill:hover { color: var(--color-error); }

  .bp-output, .bp-transcript {
    margin: 0;
    padding: 10px 12px;
    border-top: 1px solid var(--color-hairline);
    background: var(--color-code-bg);
    color: var(--color-code-text);
    font-family: var(--font-mono);
    font-size: 10.5px;
    line-height: 1.6;
    white-space: pre-wrap;
    max-height: 40vh;
    overflow: auto;
  }
  .bp-transcript { flex: 1; min-height: 0; max-height: none; }

  .bp-empty {
    margin: 12px 2px;
    color: var(--color-muted);
    font-size: 11.5px;
  }
</style>
