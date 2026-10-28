<script lang="ts">
  import { onMount, tick } from "svelte";
  import { confirmState, resolveConfirm } from "../stores/confirm";
  import { localT } from "../i18n";

  let dialogEl: HTMLDivElement | undefined;
  let confirmBtn: HTMLButtonElement | undefined;
  let lastFocused: Element | null = null;

  // Focus management follows ApprovalModal: focus the confirm action on open
  // and hand focus back to the element that triggered it. `lastFocused` is
  // re-sampled on EVERY open (not only when empty), so a second request
  // restores focus to its own trigger instead of the first one's (C11).
  let wasOpen = false;
  $: if ($confirmState.open && !wasOpen) {
    wasOpen = true;
    lastFocused = document.activeElement;
    tick().then(() => confirmBtn?.focus());
  } else if (!$confirmState.open && wasOpen) {
    wasOpen = false;
  }

  function close(ok: boolean) {
    resolveConfirm(ok);
    if (lastFocused instanceof HTMLElement) lastFocused.focus();
    lastFocused = null;
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      // stopPropagation as well: App.svelte's global handler also listens for
      // Escape and closes the side panel, so one keypress cancelled the
      // confirmation AND closed the panel (review C8).
      e.stopPropagation();
      close(false);
    } else if (e.key === "Enter") {
      e.preventDefault();
      e.stopPropagation();
      close(true);
    }
  }

  onMount(() => {
    const onGlobalKey = (e: KeyboardEvent) => {
      if ($confirmState.open) onKeydown(e);
    };
    window.addEventListener("keydown", onGlobalKey, true);
    return () => window.removeEventListener("keydown", onGlobalKey, true);
  });
</script>

{#if $confirmState.open}
  <div
    class="cf-backdrop"
    role="presentation"
    on:click|self={() => close(false)}
  >
    <div
      class="cf-box"
      bind:this={dialogEl}
      role="alertdialog"
      aria-modal="true"
      aria-labelledby="cf-msg"
      tabindex="-1"
    >
      <p id="cf-msg" class="cf-msg">{$confirmState.message}</p>
      <div class="cf-actions">
        <button class="cf-btn" on:click={() => close(false)}>
          {localT("common.cancel", "Cancel")}
        </button>
        <button
          class="cf-btn primary"
          class:danger={$confirmState.danger}
          bind:this={confirmBtn}
          on:click={() => close(true)}
        >
          {$confirmState.confirmLabel ?? localT("common.confirm", "Confirm")}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .cf-backdrop {
    background: rgba(0, 0, 0, 0.5);
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.55);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 9000;
  }
  .cf-box {
    border-radius: 16px;
    border: 1px solid var(--color-hairline-strong);
    box-shadow: 0 30px 80px -20px rgba(0,0,0,.85);
    padding: 22px 22px 18px;
    background: var(--color-surface-elevated);
    border: 1px solid var(--color-hairline-strong);
    border-radius: 10px;
    padding: 20px 22px 16px;
    max-width: 460px;
    box-shadow: 0 18px 48px rgba(0, 0, 0, 0.5);
  }
  .cf-msg {
    margin: 0 0 16px;
    color: var(--color-body);
    font-size: 13.5px;
    line-height: 1.6;
    white-space: pre-wrap;
  }
  .cf-actions {
    display: flex;
    justify-content: flex-end;
    gap: 10px;
  }
  .cf-btn {
    background: var(--color-surface-soft);
    /* The label uses --color-ink, not the accent: measured on the real tokens,
       accent-on-surface is 3.94 (danger, dark) / 3.11 (primary, light) — below
       the 4.5:1 AA floor for 13px text, and filling the button instead only
       reaches 3.90/3.22. The accent therefore carries the meaning on the
       BORDER (3.1–9.1:1, fine for non-text UI) while the words stay readable;
       the action name itself says what the button does, so nothing depends on
       hue alone. */
    color: var(--color-ink);
    border: 1px solid var(--color-hairline);
    border-radius: 6px;
    padding: 6px 14px;
    font-size: 13px;
    cursor: pointer;
  }
  .cf-btn.primary {
    border-color: var(--color-primary);
  }
  .cf-btn.primary.danger {
    border-color: var(--color-error);
  }
  .cf-btn:focus-visible {
    outline: 2px solid var(--color-primary);
    outline-offset: 2px;
  }
</style>
