<script lang="ts">
  /**
   * 导入会话 / Import sessions — imports local ZCode or DSH (DeepSeek Harness)
   * chat sessions into OpenZen.
   *
   * Backend contract (Tauri commands, implemented in src-tauri/):
   *   scan_import_sources()                 -> { sources: ImportSource[] }
   *   import_list_sessions({ source })      -> { source, error, sessions }
   *   import_sessions({ source, ids })      -> { imported, errors }
   *
   * Design: Song Celadon (frontends/DESIGN.md). Surface steps + 1px hairlines
   * only — no shadows anywhere inside the dialog.
   */
  import { tick } from "svelte";
  import { isTauri, tauriInvoke } from "../api/tauri";
  import { sessions } from "../stores/sessions";
  import { t, locale, localT } from "../i18n";

  interface Props {
    open: boolean;
    onClose: () => void;
  }

  let { open, onClose }: Props = $props();

  interface ImportSource {
    id: string;
    label: string;
    available: boolean;
    detail: string | null;
    session_count: number | null;
    error: string | null;
  }

  interface ImportSession {
    source_id: string;
    title: string;
    directory: string | null;
    created_at: string;
    message_count: number;
    already_imported: boolean;
  }

  interface SourceList {
    loading: boolean;
    loaded: boolean;
    error: string | null;
    sessions: ImportSession[];
  }

  interface ImportedEntry {
    source_id: string;
    session_id: string;
    message_count: number;
  }

  interface FailedEntry {
    source_id: string;
    error: string;
  }

  interface ImportResult {
    imported: ImportedEntry[];
    errors: FailedEntry[];
  }

  let sources = $state<ImportSource[]>([]);
  let sourcesLoading = $state(false);
  let sourcesError = $state("");
  let activeSource = $state<string | null>(null);
  /** Per-source session lists — the cache that keeps tab switching free. */
  let lists = $state<Record<string, SourceList>>({});
  /** Per-source selection, keyed by source id. */
  let selection = $state<Record<string, string[]>>({});

  let importing = $state(false);
  let result = $state<ImportResult | null>(null);
  let importError = $state("");

  let dialogEl: HTMLElement | undefined = $state();
  /** Plain closure flag: opening/closing is an edge, not a state mirror. */
  let wasOpen = false;

  const activeList = $derived(activeSource ? lists[activeSource] : undefined);
  const rows = $derived(activeList?.sessions ?? []);
  const selectedIds = $derived(activeSource ? selection[activeSource] ?? [] : []);
  const selectedCount = $derived(selectedIds.length);
  const unavailableSources = $derived(sources.filter((s) => !s.available));

  const importedCount = $derived(result ? result.imported.length : 0);
  const failedCount = $derived(result ? result.errors.length : 0);

  const summaryText = $derived(
    $t("import.resultSummary")
      .replace("{imported}", String(importedCount))
      .replace("{failed}", String(failedCount)),
  );

  function errorText(err: unknown): string {
    if (err instanceof Error) return err.message;
    return String(err);
  }

  function reset() {
    sources = [];
    sourcesLoading = false;
    sourcesError = "";
    activeSource = null;
    lists = {};
    selection = {};
    importing = false;
    result = null;
    importError = "";
  }

  // Runs on the open edge only — `wasOpen` is a plain variable, so this effect
  // depends solely on `open` and can never re-trigger itself.
  $effect(() => {
    if (open && !wasOpen) {
      wasOpen = true;
      void initialize();
    } else if (!open && wasOpen) {
      wasOpen = false;
      reset();
    }
  });

  $effect(() => {
    if (open) {
      tick().then(() => dialogEl?.focus());
    }
  });

  async function initialize() {
    reset();
    if (!isTauri()) {
      // Browser preview: the Tauri IPC bridge does not exist here.
      sourcesError = localT("import.sourceUnavailable");
      return;
    }
    sourcesLoading = true;
    try {
      const res = (await tauriInvoke("scan_import_sources")) as {
        sources?: ImportSource[];
      } | null;
      sources = res?.sources ?? [];
      const first = sources.find((s) => s.available) ?? sources[0];
      if (first) {
        activeSource = first.id;
        if (first.available) await loadSource(first.id);
      }
    } catch (err) {
      sourcesError = errorText(err);
    } finally {
      sourcesLoading = false;
    }
  }

  /** Fetch one source's session list. `force` bypasses the cache (post-import
   *  refresh, where `already_imported` must flip to true). */
  async function loadSource(id: string, force = false) {
    const cached = lists[id];
    if (!force && cached?.loaded) return;
    if (!isTauri()) return;
    lists[id] = {
      loading: true,
      loaded: false,
      error: cached?.error ?? null,
      sessions: cached?.sessions ?? [],
    };
    try {
      const res = (await tauriInvoke("import_list_sessions", { source: id })) as {
        sessions?: ImportSession[];
        error?: string | null;
      } | null;
      const err = res?.error ?? null;
      lists[id] = {
        loading: false,
        // A failed listing stays uncached so returning to the tab retries.
        loaded: err === null,
        error: err,
        sessions: res?.sessions ?? [],
      };
    } catch (err) {
      lists[id] = {
        loading: false,
        loaded: false,
        error: errorText(err),
        sessions: [],
      };
    }
  }

  function selectSource(id: string) {
    const src = sources.find((s) => s.id === id);
    if (!src || !src.available || importing) return;
    activeSource = id;
    result = null;
    importError = "";
    void loadSource(id);
  }

  function isSelected(id: string): boolean {
    return selectedIds.includes(id);
  }

  function toggleRow(id: string) {
    if (importing || !activeSource) return;
    const cur = selection[activeSource] ?? [];
    selection[activeSource] = cur.includes(id)
      ? cur.filter((x) => x !== id)
      : [...cur, id];
  }

  /** Select-all skips `already_imported` rows so one click + 导入 cannot
   *  duplicate an existing OpenZen session. Such rows stay manually tickable. */
  function selectAll() {
    if (!activeSource || importing) return;
    selection[activeSource] = rows
      .filter((r) => !r.already_imported)
      .map((r) => r.source_id);
  }

  function clearSelection() {
    if (!activeSource || importing) return;
    selection[activeSource] = [];
  }

  async function runImport() {
    const src = activeSource;
    if (!src || importing || selectedIds.length === 0) return;
    importing = true;
    importError = "";
    result = null;
    const ids = [...selectedIds];
    try {
      const res = (await tauriInvoke("import_sessions", {
        source: src,
        ids,
      })) as ImportResult | null;
      result = {
        imported: res?.imported ?? [],
        errors: res?.errors ?? [],
      };
      selection[src] = [];
      // Re-list so the freshly imported rows show the 已导入 badge, then
      // refresh the sidebar store so the new sessions appear immediately.
      await loadSource(src, true);
      await sessions.load();
    } catch (err) {
      importError = errorText(err);
    } finally {
      importing = false;
    }
  }

  function close() {
    if (importing) return;
    onClose();
  }

  function onBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget) close();
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape" && open) {
      e.preventDefault();
      e.stopPropagation();
      close();
    }
  }

  function trapFocus(e: KeyboardEvent) {
    if (e.key !== "Tab" || !dialogEl) return;
    const focusables = Array.from(
      dialogEl.querySelectorAll<HTMLElement>(
        'button:not([disabled]), input:not([disabled]), [href], [tabindex]:not([tabindex="-1"])',
      ),
    );
    if (focusables.length === 0) return;
    const first = focusables[0];
    const last = focusables[focusables.length - 1];
    const active = document.activeElement;
    if (e.shiftKey && (active === first || active === dialogEl)) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && active === last) {
      e.preventDefault();
      first.focus();
    }
  }

  /** App-locale date, same shape as SessionList.formatDate. */
  function formatDate(iso: string): string {
    try {
      return new Intl.DateTimeFormat($locale, {
        year: "numeric",
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
      }).format(new Date(iso));
    } catch {
      return "";
    }
  }

  /** Drop the middle of long paths: /Users/x/…/apps/visualmusic */
  function truncateMiddle(value: string, max = 52): string {
    if (value.length <= max) return value;
    const head = Math.ceil((max - 1) / 2);
    const tail = Math.floor((max - 1) / 2);
    return `${value.slice(0, head)}…${value.slice(value.length - tail)}`;
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if open}
  <div class="import-backdrop" onclick={onBackdropClick} role="presentation">
    <div
      class="import-dialog"
      role="dialog"
      aria-modal="true"
      aria-labelledby="import-title"
      tabindex="-1"
      bind:this={dialogEl}
      onkeydown={trapFocus}
    >
      <header class="import-head">
        <h2 class="import-title" id="import-title">{$t("import.title")}</h2>
        <button
          type="button"
          class="import-close"
          aria-label={$t("import.cancel")}
          onclick={close}
          disabled={importing}
        >
          <svg width="14" height="14" viewBox="0 0 14 14" fill="none" aria-hidden="true">
            <path d="M3 3l8 8M11 3l-8 8" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>
          </svg>
        </button>
      </header>

      {#if sourcesLoading}
        <div class="import-state">{$t("import.loading")}</div>
      {:else if sourcesError}
        <div class="import-state import-error">{sourcesError}</div>
      {:else if sources.length === 0}
        <div class="import-state">{$t("import.empty")}</div>
      {:else}
        <nav class="import-tabs" aria-label={$t("import.title")}>
          {#each sources as src (src.id)}
            <button
              type="button"
              class="import-tab"
              class:on={activeSource === src.id}
              disabled={!src.available || importing}
              title={src.available
                ? src.detail ?? ""
                : `${$t("import.sourceUnavailable")}${src.error ? ` · ${src.error}` : ""}`}
              onclick={() => selectSource(src.id)}
            >
              <span class="import-tab-label">{src.label}</span>
              {#if typeof src.session_count === "number"}
                <span class="import-tab-count">{src.session_count}</span>
              {/if}
            </button>
          {/each}
        </nav>

        {#if unavailableSources.length > 0}
          <div class="import-hints">
            {#each unavailableSources as src (src.id)}
              <div class="import-hint">
                <span class="import-hint-label">{src.label}</span>
                <span class="import-hint-text">
                  {src.error || $t("import.sourceUnavailable")}
                </span>
              </div>
            {/each}
          </div>
        {/if}

        <div class="import-toolbar">
          <button
            type="button"
            class="import-link"
            onclick={selectAll}
            disabled={importing || rows.length === 0}
          >
            {$t("import.selectAll")}
          </button>
          <button
            type="button"
            class="import-link"
            onclick={clearSelection}
            disabled={importing || selectedCount === 0}
          >
            {$t("import.clear")}
          </button>
          <span class="import-count">
            {$t("import.selected").replace("{n}", String(selectedCount))}
          </span>
        </div>

        <div class="import-body">
          {#if !activeSource}
            <div class="import-state">{$t("import.empty")}</div>
          {:else if activeList?.loading}
            <div class="import-state">{$t("import.loading")}</div>
          {:else if activeList?.error}
            <div class="import-state import-error">{activeList.error}</div>
          {:else if rows.length === 0}
            <div class="import-state">{$t("import.empty")}</div>
          {:else}
            <ul class="import-rows">
              {#each rows as row (row.source_id)}
                <li>
                  <label class="import-row" class:selected={isSelected(row.source_id)}>
                    <input
                      type="checkbox"
                      class="import-check"
                      checked={isSelected(row.source_id)}
                      disabled={importing}
                      onchange={() => toggleRow(row.source_id)}
                    />
                    <span class="import-row-main">
                      <span class="import-row-head">
                        <span
                          class="import-row-title"
                          title={row.title || row.source_id}
                        >{row.title || row.source_id}</span>
                        {#if row.already_imported}
                          <span class="import-badge">{$t("import.alreadyImported")}</span>
                        {/if}
                      </span>
                      <span class="import-row-meta">
                        {#if row.directory}
                          <span class="import-dir" title={row.directory}>
                            {truncateMiddle(row.directory)}
                          </span>
                        {/if}
                        <span class="import-date">{formatDate(row.created_at)}</span>
                        <span class="import-msgs">
                          {$t("import.messages").replace("{n}", String(row.message_count))}
                        </span>
                      </span>
                    </span>
                  </label>
                </li>
              {/each}
            </ul>
          {/if}
        </div>
      {/if}

      {#if importError || result}
        <div class="import-result" aria-live="polite">
          {#if importError}
            <div class="import-error">{importError}</div>
          {/if}
          {#if result}
            <div class="import-summary">{summaryText}</div>
            {#if failedCount > 0}
              <div class="import-fail-head">{$t("import.failed")}</div>
              <ul class="import-failures">
                {#each result.errors as failure (failure.source_id)}
                  <li class="import-failure">
                    <span class="import-fail-id">{failure.source_id}</span>
                    <span class="import-fail-msg">{failure.error}</span>
                  </li>
                {/each}
              </ul>
            {/if}
          {/if}
        </div>
      {/if}

      <footer class="import-foot">
        <button
          type="button"
          class="import-btn secondary"
          onclick={close}
          disabled={importing}
        >
          {$t("import.cancel")}
        </button>
        <button
          type="button"
          class="import-btn primary"
          onclick={runImport}
          disabled={importing || selectedCount === 0}
        >
          {importing ? $t("import.importing") : $t("import.confirm")}
        </button>
      </footer>
    </div>
  </div>
{/if}

<style>
  /* Scrim uses the same ink-night wash as the other modals (SettingsPanel):
     a scrim, not a palette colour. */
  .import-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(20, 18, 14, 0.55);
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 24px;
    z-index: 1000;
    animation: import-fade-in 0.15s ease-out;
  }

  @keyframes import-fade-in {
    from { opacity: 0; }
    to { opacity: 1; }
  }

  /* Depth = surface step + hairline. No shadow. */
  .import-dialog {
    display: flex;
    flex-direction: column;
    width: 100%;
    max-width: 660px;
    max-height: calc(100vh - 48px);
    overflow: hidden;
    background: var(--color-surface-overlay);
    border: 1px solid var(--color-hairline-strong);
    border-radius: 12px;
    color: var(--color-body);
    font-family: var(--font-sans);
  }

  .import-dialog:focus {
    outline: none;
  }

  .import-head {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 14px 16px 12px;
    border-bottom: 1px solid var(--color-hairline);
  }

  .import-title {
    flex: 1;
    margin: 0;
    font-family: var(--font-serif);
    font-size: 13px;
    font-weight: 600;
    letter-spacing: 0.25em;
    color: var(--color-ink);
  }

  .import-close {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    border: none;
    border-radius: 6px;
    background: none;
    color: var(--color-muted);
    cursor: pointer;
    transition: background 0.15s, color 0.15s;
  }

  .import-close:hover:not(:disabled) {
    background: var(--color-surface-soft);
    color: var(--color-ink);
  }

  .import-close:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }

  .import-tabs {
    display: flex;
    gap: 2px;
    padding: 10px 12px 0;
  }

  .import-tab {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 12px;
    border: 1px solid transparent;
    border-bottom: none;
    border-radius: 6px 6px 0 0;
    background: none;
    color: var(--color-muted);
    font-family: inherit;
    font-size: 11.5px;
    letter-spacing: 0.08em;
    cursor: pointer;
    transition: background 0.2s, color 0.2s, border-color 0.2s;
  }

  .import-tab:hover:not(:disabled) {
    color: var(--color-ink);
  }

  .import-tab.on {
    color: var(--color-primary);
    background: var(--color-surface-soft);
    border-color: var(--color-hairline);
  }

  .import-tab:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .import-tab-count {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--color-dim);
    font-variant-numeric: tabular-nums;
  }

  .import-hints {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 8px 12px 0;
  }

  .import-hint {
    display: flex;
    gap: 8px;
    font-size: 11px;
    color: var(--color-dim);
    line-height: 1.45;
  }

  .import-hint-label {
    flex-shrink: 0;
    color: var(--color-muted);
  }

  .import-hint-text {
    min-width: 0;
    word-break: break-word;
  }

  .import-toolbar {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 14px 8px;
    border-bottom: 1px solid var(--color-hairline);
  }

  .import-link {
    padding: 0;
    border: none;
    background: none;
    color: var(--color-primary);
    font-family: inherit;
    font-size: 11.5px;
    cursor: pointer;
    transition: color 0.2s;
  }

  .import-link:hover:not(:disabled) {
    color: var(--color-primary-hover);
  }

  .import-link:disabled {
    color: var(--color-muted);
    opacity: 0.5;
    cursor: not-allowed;
  }

  .import-count {
    margin-left: auto;
    font-size: 11px;
    color: var(--color-dim);
    font-variant-numeric: tabular-nums;
  }

  .import-body {
    flex: 1;
    min-height: 140px;
    overflow-y: auto;
    padding: 8px 10px 10px;
  }

  .import-state {
    padding: 28px 12px;
    text-align: center;
    font-size: 12px;
    color: var(--color-muted);
  }

  .import-state.import-error {
    color: var(--color-error);
    word-break: break-word;
  }

  .import-rows {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .import-row {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 7px 9px;
    border: 1px solid transparent;
    border-radius: 6px;
    cursor: pointer;
    transition: background 0.15s, border-color 0.15s;
  }

  .import-row:hover {
    background: var(--color-surface-soft);
    border-color: var(--color-hairline);
  }

  .import-row.selected {
    background: var(--color-primary-muted);
    border-color: var(--color-hairline-strong);
  }

  .import-check {
    flex-shrink: 0;
    margin: 2px 0 0;
    accent-color: var(--color-primary);
    cursor: pointer;
  }

  .import-row-main {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    flex: 1;
  }

  .import-row-head {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }

  .import-row-title {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12.5px;
    color: var(--color-ink);
  }

  .import-badge {
    flex-shrink: 0;
    padding: 1px 6px;
    border: 1px solid var(--color-hairline);
    border-radius: 999px;
    background: var(--color-surface-soft);
    color: var(--color-dim);
    font-size: 10px;
    line-height: 1.5;
  }

  .import-row-meta {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
    font-size: 11px;
    color: var(--color-dim);
  }

  .import-dir {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--color-muted);
  }

  .import-date,
  .import-msgs {
    flex-shrink: 0;
    font-variant-numeric: tabular-nums;
  }

  .import-result {
    margin: 0 14px 10px;
    padding: 9px 11px;
    border: 1px solid var(--color-hairline);
    border-radius: 6px;
    background: var(--color-surface-soft);
  }

  .import-summary {
    font-size: 12px;
    color: var(--color-ink);
  }

  .import-error {
    font-size: 12px;
    color: var(--color-error);
    word-break: break-word;
  }

  .import-fail-head {
    margin-top: 8px;
    font-size: 10.5px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--color-error);
  }

  .import-failures {
    list-style: none;
    margin: 4px 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }

  .import-failure {
    display: flex;
    gap: 8px;
    font-size: 11px;
    color: var(--color-error);
    line-height: 1.45;
  }

  .import-fail-id {
    flex-shrink: 0;
    font-family: var(--font-mono);
  }

  .import-fail-msg {
    min-width: 0;
    word-break: break-word;
  }

  .import-foot {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    padding: 12px 14px 14px;
    border-top: 1px solid var(--color-hairline);
  }

  .import-btn {
    padding: 7px 18px;
    border-radius: 6px;
    font-family: inherit;
    font-size: 12.5px;
    font-weight: 500;
    cursor: pointer;
    transition: background 0.15s, color 0.15s, border-color 0.15s;
  }

  .import-btn.secondary {
    background: transparent;
    border: 1px solid var(--color-hairline-strong);
    color: var(--color-ink);
  }

  .import-btn.secondary:hover:not(:disabled) {
    border-color: var(--color-primary);
    color: var(--color-primary);
  }

  .import-btn.primary {
    background: var(--color-primary);
    border: 1px solid var(--color-primary);
    color: var(--color-canvas);
  }

  .import-btn.primary:hover:not(:disabled) {
    background: var(--color-primary-hover);
    border-color: var(--color-primary-hover);
  }

  .import-btn:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }

  .import-btn:focus-visible,
  .import-link:focus-visible,
  .import-tab:focus-visible,
  .import-close:focus-visible {
    outline: 1px solid var(--color-primary);
    outline-offset: 1px;
  }

  @media (prefers-reduced-motion: reduce) {
    .import-backdrop {
      animation: none;
    }
  }
</style>
