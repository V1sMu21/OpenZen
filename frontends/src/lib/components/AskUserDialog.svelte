<script lang="ts">
  import { chat, type PendingAskUser } from "../stores/chat";
  import { tick } from "svelte";
  import { t } from "../i18n";

  interface Props {
    pending: PendingAskUser;
  }

  let { pending }: Props = $props();

  let customText = $state("");
  let selectedCandidate = $state<string | null>(null);
  let textareaEl: HTMLTextAreaElement | undefined = $state();
  let submitting = $state(false);

  const hasCandidates = $derived(pending.candidates.length > 0);

  $effect(() => {
    if (pending) {
      customText = "";
      selectedCandidate = null;
      submitting = false;
      tick().then(() => textareaEl?.focus());
    }
  });

  function pickCandidate(value: string) {
    selectedCandidate = value;
    customText = value;
  }

  function onTextInput() {
    if (customText && selectedCandidate && customText !== selectedCandidate) {
      selectedCandidate = null;
    }
  }

  async function submit() {
    const text = customText.trim();
    if (!text || submitting) return;
    submitting = true;
    try {
      await chat.submitAskUserResponse(text);
    } finally {
      submitting = false;
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      submit();
    } else if (e.key === "Escape") {
      e.preventDefault();
      chat.dismissAskUser();
    }
  }

  function onBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget) {
      chat.dismissAskUser();
    }
  }

  // Focus trap: aria-modal declared a modal, but Tab could escape to the
  // background page. Wrap focus inside the dialog and return it to the
  // chat input when the dialog closes.
  let dialogEl: HTMLElement | undefined = $state();

  function trapFocus(e: KeyboardEvent) {
    if (e.key !== "Tab" || !dialogEl) return;
    const focusables = Array.from(
      dialogEl.querySelectorAll<HTMLElement>(
        'button:not([disabled]), textarea:not([disabled]), input:not([disabled]), [href], [tabindex]:not([tabindex="-1"])',
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

  $effect(() => {
    const chatInput = document.querySelector<HTMLElement>(".input-area textarea");
    return () => {
      chatInput?.focus();
    };
  });
</script>

<div class="ask-backdrop" onclick={onBackdropClick} role="presentation">
  <div
    class="ask-dialog"
    role="dialog"
    aria-modal="true"
    aria-labelledby="ask-question"
    bind:this={dialogEl}
    onkeydown={trapFocus}
    tabindex="-1"
  >
    <div class="ask-header">
      <span class="ask-icon" aria-hidden="true">?</span>
      <div class="ask-title-block">
        <h2 class="ask-title" id="ask-question">{$t("ask.title")}</h2>
        <p class="ask-subtitle">{$t("ask.subtitle")}</p>
      </div>
      <button
        type="button"
        class="ask-close"
        aria-label={$t("ask.dismiss")}
        onclick={() => chat.dismissAskUser()}
      >
        <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
          <path d="M3 3l8 8M11 3l-8 8" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>
        </svg>
      </button>
    </div>

    <div class="ask-body">
      <div class="ask-question">{pending.question}</div>

      {#if hasCandidates}
        <div class="ask-candidates-label">{$t("ask.suggestions")}</div>
        <div class="ask-candidates">
          {#each pending.candidates as c, i (c + i)}
            <button
              type="button"
              class="ask-candidate"
              class:selected={selectedCandidate === c}
              onclick={() => pickCandidate(c)}
            >
              {c}
            </button>
          {/each}
        </div>
      {/if}

      <label class="ask-input-label" for="ask-custom">{$t("ask.yourResponse")}</label>
      <textarea
        id="ask-custom"
        bind:this={textareaEl}
        bind:value={customText}
        oninput={onTextInput}
        onkeydown={onKeydown}
        placeholder={$t("ask.placeholder")}
        rows="3"
        disabled={submitting}
      ></textarea>
    </div>

    <div class="ask-footer">
      <button
        type="button"
        class="ask-btn-secondary"
        onclick={() => chat.dismissAskUser()}
        disabled={submitting}
      >
        {$t("ask.cancel")}
      </button>
      <button
        type="button"
        class="ask-btn-primary"
        onclick={submit}
        disabled={!customText.trim() || submitting}
      >
        {submitting ? $t("ask.sending") : $t("ask.send")}
      </button>
    </div>
  </div>
</div>

<style>
  .ask-backdrop {
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
    animation: ask-fade-in 0.15s ease-out;
  }

  @keyframes ask-fade-in {
    from { opacity: 0; }
    to { opacity: 1; }
  }

  @keyframes ask-pop-in {
    from {
      opacity: 0;
      transform: translateY(8px) scale(0.98);
    }
    to {
      opacity: 1;
      transform: translateY(0) scale(1);
    }
  }

  .ask-dialog {
    background: var(--color-surface);
    border: 1px solid var(--color-hairline);
    border-radius: 12px;
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.4), 0 2px 8px rgba(0, 0, 0, 0.2);
    width: 100%;
    max-width: 520px;
    max-height: calc(100vh - 48px);
    display: flex;
    flex-direction: column;
    overflow: hidden;
    animation: ask-pop-in 0.18s ease-out;
    font-family: var(--font-sans, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif);
  }

  .ask-header {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    padding: 18px 18px 14px;
    border-bottom: 1px solid var(--color-hairline);
  }

  .ask-icon {
    flex-shrink: 0;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    background: var(--color-primary-muted);
    color: var(--color-primary);
    display: flex;
    align-items: center;
    justify-content: center;
    font-weight: 600;
    font-size: 15px;
    line-height: 1;
  }

  .ask-title-block {
    flex: 1;
    min-width: 0;
  }

  .ask-title {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
    color: var(--color-ink);
    line-height: 1.35;
  }

  .ask-subtitle {
    margin: 3px 0 0;
    font-size: 12px;
    color: var(--color-dim);
    line-height: 1.4;
  }

  .ask-close {
    flex-shrink: 0;
    background: transparent;
    border: none;
    color: var(--color-dim);
    cursor: pointer;
    padding: 4px;
    border-radius: 4px;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: background 0.1s, color 0.1s;
  }

  .ask-close:hover {
    background: var(--color-hairline);
    color: var(--color-ink);
  }

  .ask-body {
    padding: 16px 18px 18px;
    overflow-y: auto;
    flex: 1;
  }

  .ask-question {
    background: var(--color-surface-soft);
    border: 1px solid var(--color-hairline);
    border-radius: 8px;
    padding: 12px 14px;
    font-size: 13.5px;
    line-height: 1.55;
    color: var(--color-ink);
    white-space: pre-wrap;
    word-break: break-word;
  }

  .ask-candidates-label {
    margin: 16px 0 8px;
    font-size: 10.5px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--color-dim);
  }

  .ask-candidates {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .ask-candidate {
    background: var(--color-surface-soft);
    border: 1px solid var(--color-hairline);
    color: var(--color-ink);
    border-radius: 999px;
    padding: 6px 12px;
    font-size: 12.5px;
    font-family: inherit;
    cursor: pointer;
    transition: background 0.1s, border-color 0.1s, color 0.1s;
  }

  .ask-candidate:hover {
    border-color: var(--color-primary);
    color: var(--color-primary);
  }

  .ask-candidate.selected {
    background: var(--color-primary-muted);
    border-color: var(--color-primary);
    color: var(--color-primary);
  }

  .ask-input-label {
    display: block;
    margin: 16px 0 6px;
    font-size: 10.5px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--color-dim);
  }

  textarea {
    width: 100%;
    background: var(--color-canvas);
    border: 1px solid var(--color-hairline);
    border-radius: 8px;
    padding: 10px 12px;
    color: var(--color-ink);
    font-family: inherit;
    font-size: 13px;
    line-height: 1.5;
    resize: vertical;
    min-height: 72px;
    max-height: 200px;
    outline: none;
    transition: border-color 0.12s;
    box-sizing: border-box;
  }

  textarea:focus {
    border-color: var(--color-primary);
  }

  textarea:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .ask-footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    padding: 12px 18px 14px;
    border-top: 1px solid var(--color-hairline);
  }

  .ask-btn-secondary,
  .ask-btn-primary {
    font-family: inherit;
    font-size: 12.5px;
    font-weight: 500;
    padding: 7px 14px;
    border-radius: 6px;
    cursor: pointer;
    transition: background 0.1s, color 0.1s, border-color 0.1s, opacity 0.1s;
  }

  .ask-btn-secondary {
    background: transparent;
    border: 1px solid var(--color-hairline);
    color: var(--color-dim);
  }

  .ask-btn-secondary:hover:not(:disabled) {
    border-color: var(--color-dim);
    color: var(--color-ink);
  }

  .ask-btn-primary {
    background: var(--color-primary);
    border: 1px solid var(--color-primary);
    color: #0a0a0b;
  }

  .ask-btn-primary:hover:not(:disabled) {
    filter: brightness(1.08);
  }

  .ask-btn-primary:disabled,
  .ask-btn-secondary:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
</style>
