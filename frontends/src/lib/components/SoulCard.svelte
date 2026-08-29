<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { isTauri, tauriInvoke } from "../api/tauri";
  import { birthNameDisplay, soulDisplayName } from "../api/settings";
  import { soulStore } from "../stores/soul.svelte";
  import { t } from "../i18n";

  /** Mirrors the get_memory_status command response (commands.rs). */
  interface SoulStatus {
    enabled: boolean;
    embedding_kind?: string;
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

  // Shared store: a rename in the settings panel updates this card instantly
  // (and vice versa) instead of waiting for the 30s poll or a remount.
  let status = $derived(soulStore.status);
  let expanded = $state(false);
  let timer: ReturnType<typeof setInterval> | null = null;

  // ── Agent name (user-given) ──
  // soul.identity is the name source; soulDisplayName returns it only once
  // the user actually named the agent — the engine auto-fills a birth name
  // or the "未命名的记忆体" placeholder before that, which falls back to
  // the generic "灵魂" title.
  let agentName = $derived(soulDisplayName(status));

  /** Locale-aware identity display: the engine's auto birth name is
   *  Chinese data ("记忆体 · 醒于 …") — re-render per locale; anything
   *  else (user-given name) shows as-is. */
  function identityDisplay(id: string | undefined): string {
    if (!id?.trim()) return $t("soul.unnamed");
    return birthNameDisplay(id, $t("soul.birthName")) ?? id;
  }

  let renaming = $state(false);
  let nameDraft = $state("");
  let savingName = $state(false);

  function focusOnMount(node: HTMLInputElement) {
    node.focus();
    node.select();
  }

  function startRename() {
    nameDraft = agentName ?? "";
    renaming = true;
  }

  async function saveName() {
    const name = nameDraft.trim();
    if (!name || savingName) return;
    savingName = true;
    try {
      await tauriInvoke("set_soul_identity", { name });
      renaming = false;
      await refresh();
    } catch {
      // keep the editor open so the user can retry
    } finally {
      savingName = false;
    }
  }

  async function refresh() {
    if (!isTauri()) return;
    try {
      const s = (await tauriInvoke("get_memory_status")) as SoulStatus;
      // Module-level store: safe to write even if unmounted mid-invoke.
      soulStore.set(s);
      if (!s.enabled && timer) {
        clearInterval(timer); // file backend: nothing to poll
        timer = null;
      }
    } catch {
      // keep the previous snapshot on IPC failure
    }
  }

  onMount(() => {
    // One fetch so the collapsed header has data; ongoing polling only
    // runs while the panel is expanded (see effect below).
    if (isTauri()) refresh();
  });
  onDestroy(() => {
    if (timer) clearInterval(timer);
  });

  // P1-g: poll only while expanded — the collapsed header shows cached
  // totals, and a permanent 30s IPC loop burned battery for data nobody
  // was looking at.
  $effect(() => {
    if (!isTauri() || !expanded) return;
    refresh();
    timer = setInterval(refresh, 30_000);
    return () => {
      if (timer) clearInterval(timer);
      timer = null;
    };
  });

  const fmtBytes = (b: number) =>
    b >= 1048576 ? `${(b / 1048576).toFixed(1)} MB` : b >= 1024 ? `${(b / 1024).toFixed(0)} KB` : `${b} B`;
  const pct = (x: number) => `${(x * 100).toFixed(0)}%`;

  let rows = $derived(
    status?.enabled && status.soul && status.store
      ? [
          { key: "soul.identity", value: identityDisplay(status.soul.identity) },
          { key: "soul.mood", value: status.soul.mood },
          ...(status.embedding_kind
            ? [{ key: "soul.embeddings", value: status.embedding_kind }]
            : []),
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
      <span class="soul-label">{agentName ?? $t("soul.title")}</span>
      <span class="soul-detail">
        {status.store?.total_entries ?? 0}{#if !agentName} · {identityDisplay(status.soul?.identity)}{/if}
      </span>
    </button>

    {#if expanded}
      <div class="soul-list">
        {#each rows as row (row.key)}
          <div class="soul-row">
            <span class="soul-key">{$t(row.key)}</span>
            {#if row.key === "soul.identity" && renaming}
              <input
                class="soul-name-input"
                bind:value={nameDraft}
                use:focusOnMount
                placeholder={$t("soul.namePlaceholder")}
                maxlength={24}
                disabled={savingName}
                onkeydown={(e) => {
                  if (e.key === "Enter") saveName();
                  else if (e.key === "Escape") renaming = false;
                }}
                onblur={() => {
                  // 失焦也保存 — 只按 Enter 会让人 "输了名字没生效"
                  if (renaming) saveName();
                }}
              />
              <button class="soul-rename-btn" onclick={saveName} disabled={savingName} title={$t("soul.rename")}>✓</button>
            {:else if row.key === "soul.identity"}
              <span class="soul-val">{row.value}</span>
              <button class="soul-rename-btn" onclick={startRename} title={$t("soul.rename")}>
                <svg width="11" height="11" viewBox="0 0 12 12" fill="none">
                  <path d="M8.3 1.7l2 2L4 10l-2.6.6L2 8l6.3-6.3z" stroke="currentColor" stroke-width="1.1" stroke-linecap="round" stroke-linejoin="round"/>
                </svg>
              </button>
            {:else}
              <span class="soul-val">{row.value}</span>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </div>
{/if}

<style>
  .soul-card {
    width: 320px;
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
    /* 占满折叠头剩余宽度；放不下时换行而不是截断，
       保证 "记忆体 · 醒于 …" 全文可见 */
    margin-left: auto;
    flex: 1 1 auto;
    min-width: 0;
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    opacity: 0.7;
    word-break: break-word;
    text-align: right;
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
    /* 长值（如 identity）换行展示全部内容，不再单行截断 */
    word-break: break-word;
  }

  .soul-rename-btn {
    flex: 0 0 auto;
    display: inline-flex;
    align-items: center;
    border: none;
    background: none;
    padding: 0 2px;
    color: var(--color-muted);
    cursor: pointer;
    opacity: 0.6;
    transition: opacity 0.15s, color 0.15s;
  }

  .soul-rename-btn:hover {
    opacity: 1;
    color: var(--color-primary);
  }

  .soul-rename-btn:disabled {
    cursor: default;
    opacity: 0.4;
  }

  .soul-name-input {
    flex: 1 1 auto;
    min-width: 0;
    font-family: inherit;
    font-size: 12px;
    padding: 2px 6px;
    border: 1px solid var(--color-hairline-strong);
    border-radius: 4px;
    background: var(--color-surface-soft);
    color: var(--color-ink);
    text-align: right;
  }

  .soul-name-input:focus {
    outline: none;
    border-color: var(--color-primary);
  }

  /* 窄窗口下与 todo-rail 同步隐藏 */
  @media (max-width: 1100px) {
    .soul-card {
      display: none;
    }
  }
</style>
