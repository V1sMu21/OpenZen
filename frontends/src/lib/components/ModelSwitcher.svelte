<script lang="ts">
  import { chat } from "../stores/chat";
  import { listModels, type ModelEntry } from "../api/chat";
  import { t } from "../i18n";

  let models: ModelEntry[] = $state([]);
  let loading = $state(true);
  let switching = $state<string | null>(null);

  $effect(() => {
    loadModels();
  });

  async function loadModels() {
    loading = true;
    const list = await listModels();
    models = list;
    chat.setModelList(list);
    loading = false;
  }

  function isActive(m: ModelEntry): boolean {
    return m.name === $chat.selectedModel;
  }

  async function switchTo(m: ModelEntry) {
    switching = m.name;
    chat.setSelectedModel(m.name);
    // Give user a brief visual confirmation before closing
    await new Promise((r) => setTimeout(r, 300));
    close();
  }

  function close() {
    chat.closeModelSwitcher();
  }

  function onBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget) close();
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      close();
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<div class="switcher-backdrop" onclick={onBackdropClick} role="presentation">
  <div class="switcher-dialog" role="dialog" aria-modal="true" aria-labelledby="switcher-title">
    <div class="switcher-header">
      <div class="switcher-title-block">
        <h2 class="switcher-title" id="switcher-title">{$t("model.title")}</h2>
        <p class="switcher-subtitle">{$t("model.subtitle")}</p>
      </div>
      <button
        type="button"
        class="switcher-close"
        aria-label={$t("model.close")}
        onclick={close}
      >
        <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
          <path d="M3 3l8 8M11 3l-8 8" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>
        </svg>
      </button>
    </div>

    <div class="switcher-body">
      {#if loading}
        <div class="switcher-loading">{$t("model.loading")}</div>
      {:else if models.length === 0}
        <div class="switcher-empty">
          <p>{$t("model.noModels")}</p>
          <p class="switcher-hint">{@html $t("model.checkConfig")}</p>
        </div>
      {:else}
        <div class="switcher-list">
          {#each models as m (m.name)}
            <button
              type="button"
              class="switcher-item"
              class:active={isActive(m)}
              class:switching={switching === m.name}
              disabled={isActive(m) || switching !== null}
              onclick={() => switchTo(m)}
            >
              <div class="switcher-item-left">
                <span class="switcher-item-name">{m.name}</span>
                <span class="switcher-item-model">{m.model}</span>
              </div>
              <div class="switcher-item-right">
                <span class="switcher-item-provider" class:local={m.provider === "openai"} class:online={m.provider === "claude"}>
                  {m.provider === "openai" ? "Local" : "Online"}
                </span>
                <span class="switcher-item-ctx" title="Context window size">
                  {m.context_win >= 1000000
                    ? `${(m.context_win / 1000000).toFixed(1)}M`
                    : m.context_win >= 1000
                      ? `${(m.context_win / 1000).toFixed(0)}K`
                      : m.context_win.toLocaleString()} ctx
                </span>
                {#if isActive(m)}
                  <span class="switcher-item-check">✓</span>
                {/if}
              </div>
            </button>
          {/each}
        </div>
      {/if}
    </div>

    <div class="switcher-footer">
      <button type="button" class="switcher-btn-secondary" onclick={close}>
        Cancel
      </button>
    </div>
  </div>
</div>

<style>
  .switcher-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    backdrop-filter: blur(4px);
    -webkit-backdrop-filter: blur(4px);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
    padding: 24px;
    animation: fade-in 0.15s ease-out;
  }

  @keyframes fade-in {
    from { opacity: 0; }
    to { opacity: 1; }
  }

  @keyframes pop-in {
    from {
      opacity: 0;
      transform: translateY(8px) scale(0.98);
    }
    to {
      opacity: 1;
      transform: translateY(0) scale(1);
    }
  }

  .switcher-dialog {
    background: var(--color-surface, #1a1a1c);
    border: 1px solid var(--color-hairline, #2a2a2e);
    border-radius: 12px;
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.4), 0 2px 8px rgba(0, 0, 0, 0.2);
    width: 100%;
    max-width: 480px;
    max-height: calc(100vh - 48px);
    display: flex;
    flex-direction: column;
    overflow: hidden;
    animation: pop-in 0.18s ease-out;
    font-family: var(--font-sans, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif);
  }

  .switcher-header {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    padding: 18px 18px 14px;
    border-bottom: 1px solid var(--color-hairline, #2a2a2e);
  }

  .switcher-title-block {
    flex: 1;
    min-width: 0;
  }

  .switcher-title {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
    color: var(--color-ink, #e8e8ea);
    line-height: 1.35;
  }

  .switcher-subtitle {
    margin: 3px 0 0;
    font-size: 12px;
    color: var(--color-dim, #8a8a90);
    line-height: 1.4;
  }

  .switcher-close {
    flex-shrink: 0;
    background: transparent;
    border: none;
    color: var(--color-dim, #8a8a90);
    cursor: pointer;
    padding: 4px;
    border-radius: 4px;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: background 0.1s, color 0.1s;
  }

  .switcher-close:hover {
    background: var(--color-hairline, #2a2a2e);
    color: var(--color-ink, #e8e8ea);
  }

  .switcher-body {
    padding: 12px 18px;
    overflow-y: auto;
    flex: 1;
  }

  .switcher-loading,
  .switcher-empty {
    text-align: center;
    padding: 32px 16px;
    color: var(--color-dim, #8a8a90);
    font-size: 13px;
  }

  .switcher-hint {
    margin-top: 8px;
    font-size: 11px;
    opacity: 0.7;
  }

  .switcher-hint code {
    font-family: var(--font-mono);
    font-size: 11px;
    padding: 1px 4px;
    border-radius: 3px;
    background: var(--color-surface-soft, #15151a);
  }

  .switcher-list {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .switcher-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    width: 100%;
    padding: 10px 12px;
    border-radius: 8px;
    border: 1px solid transparent;
    background: transparent;
    color: var(--color-ink, #e8e8ea);
    cursor: pointer;
    text-align: left;
    font-family: inherit;
    transition: background 0.1s, border-color 0.1s;
  }

  .switcher-item:hover:not(:disabled) {
    background: var(--color-surface-soft, #15151a);
    border-color: var(--color-hairline, #2a2a2e);
  }

  .switcher-item.active {
    background: color-mix(in srgb, var(--color-primary, #81b5c7) 8%, transparent);
    border-color: color-mix(in srgb, var(--color-primary, #81b5c7) 25%, transparent);
  }

  .switcher-item:disabled {
    cursor: default;
    opacity: 0.7;
  }

  .switcher-item.switching {
    opacity: 0.5;
  }

  .switcher-item-left {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    flex: 1;
  }

  .switcher-item-name {
    font-size: 13px;
    font-weight: 500;
    line-height: 1.3;
  }

  .switcher-item-model {
    font-size: 11px;
    font-family: var(--font-mono);
    color: var(--color-dim, #8a8a90);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .switcher-item-right {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
  }

  .switcher-item-provider {
    font-size: 9px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.4px;
    padding: 2px 6px;
    border-radius: 4px;
    line-height: 1.4;
  }

  .switcher-item-provider.local {
    background: color-mix(in srgb, var(--color-info, #5dade2) 15%, transparent);
    color: var(--color-info, #5dade2);
  }

  .switcher-item-provider.online {
    background: color-mix(in srgb, var(--color-accent, #81b5c7) 15%, transparent);
    color: var(--color-accent, #81b5c7);
  }

  .switcher-item-ctx {
    font-size: 10px;
    color: var(--color-dim, #8a8a90);
    font-family: var(--font-mono);
  }

  .switcher-item-check {
    font-size: 14px;
    color: var(--color-primary, #81b5c7);
    font-weight: 600;
  }

  .switcher-footer {
    display: flex;
    justify-content: flex-end;
    padding: 12px 18px 14px;
    border-top: 1px solid var(--color-hairline, #2a2a2e);
  }

  .switcher-btn-secondary {
    font-family: inherit;
    font-size: 12.5px;
    font-weight: 500;
    padding: 7px 14px;
    border-radius: 6px;
    cursor: pointer;
    transition: background 0.1s, color 0.1s, border-color 0.1s;
    background: transparent;
    border: 1px solid var(--color-hairline, #2a2a2e);
    color: var(--color-dim, #8a8a90);
  }

  .switcher-btn-secondary:hover {
    border-color: var(--color-dim, #8a8a90);
    color: var(--color-ink, #e8e8ea);
  }
</style>
