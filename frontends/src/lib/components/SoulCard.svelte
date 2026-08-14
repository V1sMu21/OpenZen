<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { isTauri, tauriInvoke } from "../api/tauri";
  import { t } from "../i18n";

  interface SoulStatus {
    enabled: boolean;
    soul?: {
      identity: string;
      mood: string;
      confidence: number;
      portrait_facts: number;
      narrative_chapters: number;
      version: number;
    };
    store?: {
      total_entries: number;
      recalls: number;
      recall_hit_rate: number;
      l3_storage_bytes: number;
    };
    harness?: { entry_count: number };
  }

  let { onVisible = (_v: boolean) => {} } = $props();

  let status = $state<SoulStatus | null>(null);
  let expanded = $state(false);
  let timer: ReturnType<typeof setInterval> | null = null;

  async function refresh() {
    if (!isTauri()) return;
    try {
      const s = (await tauriInvoke("get_memory_status")) as SoulStatus;
      status = s;
      if (s.enabled) onVisible(true);
    } catch {
      status = null;
    }
  }

  onMount(() => {
    refresh();
    timer = setInterval(refresh, 30_000);
  });
  onDestroy(() => {
    if (timer) clearInterval(timer);
  });

  const fmtBytes = (b: number) =>
    b >= 1048576 ? `${(b / 1048576).toFixed(1)} MB` : b >= 1024 ? `${(b / 1024).toFixed(0)} KB` : `${b} B`;
  const pct = (x: number) => `${(x * 100).toFixed(0)}%`;
</script>

{#if status?.enabled}
  <div class="soul-card">
    <button class="soul-toggle" onclick={() => (expanded = !expanded)} type="button">
      <svg class="soul-chevron" class:expanded width="10" height="10" viewBox="0 0 10 10" fill="none">
        <path d="M3.5 2l3.5 3-3.5 3" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/>
      </svg>
      <span class="soul-label">🧠 {$t("soul.title")}</span>
      <span class="soul-detail">{status.store?.total_entries ?? 0} · {status.soul?.identity ?? ""}</span>
    </button>

    {#if expanded}
      <div class="soul-list">
        <div class="soul-row">
          <span class="soul-key">{$t("soul.identity")}</span>
          <span class="soul-val">{status.soul?.identity}</span>
        </div>
        <div class="soul-row">
          <span class="soul-key">{$t("soul.mood")}</span>
          <span class="soul-val">{status.soul?.mood}</span>
        </div>
        <div class="soul-row">
          <span class="soul-key">{$t("soul.confidence")}</span>
          <span class="soul-val">{pct(status.soul?.confidence ?? 0)}</span>
        </div>
        <div class="soul-row">
          <span class="soul-key">{$t("soul.portraitFacts")}</span>
          <span class="soul-val">{status.soul?.portrait_facts}</span>
        </div>
        <div class="soul-row">
          <span class="soul-key">{$t("soul.narrative")}</span>
          <span class="soul-val">{status.soul?.narrative_chapters} {$t("soul.chapters")}</span>
        </div>
        <div class="soul-row">
          <span class="soul-key">{$t("soul.memories")}</span>
          <span class="soul-val">{status.store?.total_entries}</span>
        </div>
        <div class="soul-row">
          <span class="soul-key">{$t("soul.recalls")}</span>
          <span class="soul-val">{status.store?.recalls} · {$t("soul.hitRate")} {pct(status.store?.recall_hit_rate ?? 0)}</span>
        </div>
        <div class="soul-row">
          <span class="soul-key">{$t("soul.storage")}</span>
          <span class="soul-val">{fmtBytes(status.store?.l3_storage_bytes ?? 0)}</span>
        </div>
        <div class="soul-row">
          <span class="soul-key">{$t("soul.harness")}</span>
          <span class="soul-val">{status.harness?.entry_count}</span>
        </div>
      </div>
    {/if}
  </div>
{/if}

<style>
  .soul-card {
    margin: 4px 0;
  }

  .soul-toggle {
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

  .soul-toggle:hover {
    color: var(--color-body);
    border-color: var(--color-hairline-strong);
  }

  .soul-chevron {
    flex: 0 0 auto;
    color: var(--color-muted);
    transition: transform 0.15s;
  }

  .soul-chevron.expanded {
    transform: rotate(90deg);
  }

  .soul-label {
    font-weight: 600;
    color: var(--color-accent);
  }

  .soul-detail {
    margin-left: auto;
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    opacity: 0.7;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 140px;
  }

  .soul-list {
    margin-top: 4px;
    border: 1px solid var(--color-hairline);
    border-radius: 6px;
    background: var(--color-surface-soft);
    padding: 4px 8px;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }

  .soul-row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
    font-size: 12px;
  }

  .soul-key {
    color: var(--color-muted);
    font-size: 11px;
  }

  .soul-val {
    color: var(--color-body);
    font-variant-numeric: tabular-nums;
    text-align: right;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
