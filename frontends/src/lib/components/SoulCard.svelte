<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { isTauri, tauriInvoke } from "../api/tauri";
  import { t } from "../i18n";

  /** Mirrors the get_memory_status command response (commands.rs). */
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
      l1_entries: number;
      l2_entries: number;
      l3_entries: number;
      l3_storage_bytes: number;
      stores: number;
      recalls: number;
      recall_hits: number;
      recall_misses: number;
      consolidations: number;
      recall_hit_rate: number;
    };
    harness?: { entry_count: number };
  }

  let status = $state<SoulStatus | null>(null);
  let expanded = $state(false);
  let timer: ReturnType<typeof setInterval> | null = null;
  let destroyed = false;

  async function refresh() {
    if (!isTauri()) return;
    try {
      const s = (await tauriInvoke("get_memory_status")) as SoulStatus;
      if (destroyed) return; // unmounted mid-invoke — drop the write
      status = s;
      if (!s.enabled && timer) {
        clearInterval(timer); // file backend: nothing to poll
        timer = null;
      }
    } catch {
      if (!destroyed) status = null;
    }
  }

  onMount(() => {
    if (isTauri()) {
      refresh();
      timer = setInterval(refresh, 30_000);
    }
  });
  onDestroy(() => {
    destroyed = true;
    if (timer) clearInterval(timer);
  });

  const fmtBytes = (b: number) =>
    b >= 1048576 ? `${(b / 1048576).toFixed(1)} MB` : b >= 1024 ? `${(b / 1024).toFixed(0)} KB` : `${b} B`;
  const pct = (x: number) => `${(x * 100).toFixed(0)}%`;

  let rows = $derived(
    status?.enabled && status.soul && status.store
      ? [
          { key: "soul.identity", value: status.soul.identity },
          { key: "soul.mood", value: status.soul.mood },
          { key: "soul.confidence", value: pct(status.soul.confidence) },
          { key: "soul.portraitFacts", value: String(status.soul.portrait_facts) },
          { key: "soul.narrative", value: `${status.soul.narrative_chapters} ${$t("soul.chapters")}` },
          { key: "soul.memories", value: String(status.store.total_entries) },
          { key: "soul.recalls", value: `${status.store.recalls} · ${$t("soul.hitRate")} ${pct(status.store.recall_hit_rate)}` },
          { key: "soul.storage", value: fmtBytes(status.store.l3_storage_bytes) },
          { key: "soul.harness", value: String(status.harness?.entry_count ?? 0) },
        ]
      : [],
  );
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
        {#each rows as row (row.key)}
          <div class="soul-row">
            <span class="soul-key">{$t(row.key)}</span>
            <span class="soul-val">{row.value}</span>
          </div>
        {/each}
      </div>
    {/if}
  </div>
{/if}

<style>
  .soul-card {
    flex: none;
    width: 320px;
    position: sticky;
    top: 0;
    align-self: flex-start;
    margin-top: 4px;
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

  /* 窄窗口下与 todo-rail 同步隐藏 */
  @media (max-width: 1100px) {
    .soul-card {
      display: none;
    }
  }
</style>
