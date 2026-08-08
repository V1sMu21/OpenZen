<script lang="ts">
  let { show = $bindable(false) } = $props();
  import { t } from "../i18n";

  function close() {
    show = false;
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      close();
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} />

{#if show}
  <div class="shortcuts-backdrop" onclick={close} role="presentation">
    <div class="shortcuts-panel" onclick={(e) => e.stopPropagation()} role="dialog" aria-label={$t("shortcuts.title")} tabindex="0">
      <div class="shortcuts-header">
        <h2>{$t("shortcuts.title")}</h2>
        <button class="shortcuts-close" onclick={close} aria-label={$t("shortcuts.dialogClose")}>
          <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
            <path d="M3 3l8 8M11 3l-8 8" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
          </svg>
        </button>
      </div>

      <div class="shortcuts-section">
        <h3>{$t("shortcuts.global")}</h3>
        <div class="shortcut-row">
          <kbd>⌘N</kbd> <span>{$t("shortcuts.new")}</span>
        </div>
        <div class="shortcut-row">
          <kbd>⌘[</kbd> <kbd>⌘]</kbd> <span>{$t("shortcuts.prevNext")}</span>
        </div>
        <div class="shortcut-row">
          <kbd>⌘⇧S</kbd> <span>{$t("shortcuts.toggleSidebar")}</span>
        </div>
        <div class="shortcut-row">
          <kbd>⌘/</kbd> <span>{$t("shortcuts.showPanel")}</span>
        </div>
        <div class="shortcut-row">
          <kbd>⌘⇧D</kbd> <span>{$t("shortcuts.deleteSession")}</span>
        </div>
      </div>

      <div class="shortcuts-section">
        <h3>{$t("shortcuts.messages")}</h3>
        <div class="shortcut-row">
          <kbd>C</kbd> <span>{$t("shortcuts.copyLast")}</span>
        </div>
        <div class="shortcut-row">
          <kbd>↑</kbd> <span>{$t("shortcuts.focusSidebar")}</span>
        </div>
      </div>

      <div class="shortcuts-section">
        <h3>{$t("shortcuts.input")}</h3>
        <div class="shortcut-row">
          <kbd>Enter</kbd> <span>{$t("shortcuts.sendMsg")}</span>
        </div>
        <div class="shortcut-row">
          <kbd>⇧Enter</kbd> <span>{$t("shortcuts.newline")}</span>
        </div>
        <div class="shortcut-row">
          <kbd>Esc</kbd> <span>{$t("shortcuts.cancelTask")}</span>
        </div>
        <div class="shortcut-row">
          <kbd>/xxx</kbd> <span>{$t("shortcuts.command")}</span>
        </div>
      </div>

      <div class="shortcuts-section">
        <h3>{$t("shortcuts.dialog")}</h3>
        <div class="shortcut-row">
          <kbd>Esc</kbd> <span>{$t("shortcuts.dialogClose")}</span>
        </div>
        <div class="shortcut-row">
          <kbd>Enter</kbd> <span>{$t("shortcuts.dialogConfirm")}</span>
        </div>
        <div class="shortcut-row">
          <kbd>Tab</kbd> <kbd>⇧Tab</kbd> <span>{$t("shortcuts.dialogFocus")}</span>
        </div>
      </div>
    </div>
  </div>
{/if}

<style>
  .shortcuts-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
    backdrop-filter: blur(4px);
  }

  .shortcuts-panel {
    background: var(--color-surface-overlay);
    border: 1px solid var(--color-hairline-strong);
    border-radius: 12px;
    padding: 24px;
    max-width: 420px;
    width: 90vw;
    max-height: 80vh;
    overflow-y: auto;
    color: var(--color-body);
  }

  .shortcuts-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 20px;
  }

  .shortcuts-header h2 {
    margin: 0;
    font-size: 18px;
    font-weight: 600;
    color: var(--color-ink);
  }

  .shortcuts-close {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 32px;
    height: 32px;
    border-radius: 8px;
    background: none;
    border: none;
    color: var(--color-muted);
    cursor: pointer;
    transition: background 0.15s, color 0.15s;
  }

  .shortcuts-close:hover {
    background: var(--color-surface-soft);
    color: var(--color-ink);
  }

  .shortcuts-section {
    margin-bottom: 16px;
  }

  .shortcuts-section h3 {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.6px;
    color: var(--color-dim);
    margin: 0 0 8px;
    padding-bottom: 4px;
    border-bottom: 1px solid var(--color-hairline);
  }

  .shortcut-row {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 0;
    font-size: 13px;
  }

  .shortcut-row span {
    flex: 1;
  }

  kbd {
    display: inline-block;
    padding: 2px 6px;
    border-radius: 4px;
    background: var(--color-surface-elevated);
    border: 1px solid var(--color-hairline-strong);
    font-family: var(--font-mono, "JetBrains Mono", monospace);
    font-size: 11px;
    color: var(--color-ink);
    line-height: 1.4;
  }
</style>
