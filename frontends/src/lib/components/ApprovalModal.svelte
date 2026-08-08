<script lang="ts">
  import { approval } from "../stores/approval";
  import { t, localT } from "../i18n";

  function trustedMsg(approvedCount: number, currentLevel: string): string {
    return localT("approval.trusted")
      .replace("{count}", String(approvedCount))
      .replace("{level}", currentLevel);
  }

  function trustHintMsg(toolName: string, pattern: string): string {
    return localT("approval.trustHint")
      .replace("{tool}", toolName)
      .replace("{pattern}", pattern);
  }

  let { show = $approval.showModal }: { show?: boolean } = $props();
</script>

{#if $approval.showModal && $approval.current}
  {@const req = $approval.current}
  <div class="approval-overlay" role="dialog" aria-modal="true">
    <div class="approval-modal">
      <!-- Header -->
      <div class="approval-header">
        <svg width="20" height="20" viewBox="0 0 20 20" fill="none" class="approval-icon">
          <path d="M10 2a8 8 0 1 0 0 16 8 8 0 0 0 0-16zm0 3.5a.75.75 0 0 1 .75.75v4a.75.75 0 0 1-1.5 0v-4A.75.75 0 0 1 10 5.5zm0 8a.75.75 0 1 1 0-1.5.75.75 0 0 1 0 1.5z" fill="currentColor"/>
        </svg>
        <h3 class="approval-title">{$t("approval.title")}</h3>
        <span class="approval-countdown">{$approval.countdown}s</span>
      </div>

      <!-- Trusted summary -->
      <div class="approval-trust-info">
        {#if req.approvedCount > 0}
          ✅ {trustedMsg(req.approvedCount, req.currentLevel)}
        {:else}
          ⚡ {$t("approval.firstTime")}
        {/if}
      </div>

      <!-- Tool call detail -->
      <div class="approval-detail">
        <div class="approval-tool-name">{req.toolName}</div>
        <pre class="approval-args">{JSON.stringify(req.arguments, null, 2)}</pre>
      </div>

      <!-- Action buttons -->
      <div class="approval-actions">
        <button
          class="btn btn-deny"
          onclick={() => approval.respond("deny")}
          title={$t("approval.denyTitle")}
        >
          <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
            <path d="M3 3l8 8M11 3l-8 8" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
          </svg>
          {$t("approval.deny")}
        </button>

        <button
          class="btn btn-allow"
          onclick={() => approval.respond("allow")}
          title={$t("approval.allowTitle")}
        >
          <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
            <path d="M2.5 7l3 3 6-6" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
          </svg>
          {$t("approval.allow")}
        </button>

        <button
          class="btn btn-trust-session"
          onclick={() => approval.respond("trust_session")}
          title={$t("approval.trustSessionTitle")}
        >
          <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
            <path d="M2.5 7l3 3 6-6" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
            <path d="M2.5 7l3 3 6-6" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" transform="translate(0, 0)"/>
          </svg>
          {$t("approval.trustSession")}
        </button>

        <button
          class="btn btn-block"
          onclick={() => approval.respond("block_forever")}
          title={$t("approval.permDenyTitle")}
        >
          <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
            <circle cx="7" cy="7" r="5.5" stroke="currentColor" stroke-width="1.2"/>
            <path d="M4 4l6 6" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
          </svg>
          {$t("approval.permDeny")}
        </button>
      </div>

      <!-- Hint -->
      <p class="approval-hint">
        💡 {trustHintMsg(req.toolName, req.pattern)}
        {$t("approval.autoUpgrade")}
      </p>
    </div>
  </div>
{/if}

<style>
  .approval-overlay {
    position: fixed;
    inset: 0;
    z-index: 1000;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(0, 0, 0, 0.6);
    backdrop-filter: blur(4px);
    animation: fadeIn 0.15s ease;
  }

  .approval-modal {
    width: 480px;
    max-width: 90vw;
    max-height: 85vh;
    overflow-y: auto;
    background: var(--color-surface, #1a1a1a);
    border: 1px solid var(--color-hairline, #333);
    border-radius: 16px;
    padding: 24px;
    display: flex;
    flex-direction: column;
    gap: 16px;
    box-shadow: 0 24px 64px rgba(0, 0, 0, 0.5);
    animation: slideUp 0.2s ease;
  }

  .approval-header {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .approval-icon {
    color: var(--color-accent, #f0b850);
    flex-shrink: 0;
  }

  .approval-title {
    flex: 1;
    font-size: 16px;
    font-weight: 600;
    color: var(--color-ink, #e0e0e0);
    margin: 0;
  }

  .approval-countdown {
    font-size: 13px;
    font-family: var(--font-mono, monospace);
    color: var(--color-dim, #888);
    background: var(--color-surface-elevated, #262626);
    padding: 2px 8px;
    border-radius: 6px;
  }

  .approval-trust-info {
    font-size: 12px;
    color: var(--color-dim, #888);
    padding: 6px 10px;
    background: var(--color-surface-soft, #222);
    border-radius: 8px;
    border: 1px solid var(--color-hairline, #333);
  }

  .approval-detail {
    border: 1px solid var(--color-hairline, #333);
    border-radius: 10px;
    padding: 12px;
    background: var(--color-surface-soft, #222);
  }

  .approval-tool-name {
    font-family: var(--font-mono, monospace);
    font-size: 14px;
    font-weight: 600;
    color: var(--color-accent, #f0b850);
    margin-bottom: 8px;
  }

  .approval-args {
    font-family: var(--font-mono, monospace);
    font-size: 12px;
    color: var(--color-muted, #999);
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 160px;
    overflow-y: auto;
    margin: 0;
    padding: 8px;
    background: var(--color-surface, #1a1a1a);
    border-radius: 6px;
    line-height: 1.5;
  }

  .approval-actions {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
  }

  .btn {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    padding: 10px 14px;
    border-radius: 10px;
    font-size: 13px;
    font-weight: 500;
    cursor: pointer;
    border: 1px solid transparent;
    transition: all 0.15s;
  }

  .btn-deny {
    background: transparent;
    border-color: var(--color-hairline, #333);
    color: var(--color-dim, #888);
  }

  .btn-deny:hover {
    background: color-mix(in srgb, var(--color-error, #e55) 10%, transparent);
    border-color: var(--color-error, #e55);
    color: var(--color-error, #e55);
  }

  .btn-allow {
    background: color-mix(in srgb, var(--color-accent, #f0b850) 12%, transparent);
    border-color: var(--color-accent, #f0b850);
    color: var(--color-accent, #f0b850);
  }

  .btn-allow:hover {
    background: color-mix(in srgb, var(--color-accent, #f0b850) 22%, transparent);
  }

  .btn-trust-session {
    background: color-mix(in srgb, var(--color-accent, #f0b850) 18%, transparent);
    border-color: var(--color-accent, #f0b850);
    color: var(--color-accent, #f0b850);
  }

  .btn-trust-session:hover {
    background: color-mix(in srgb, var(--color-accent, #f0b850) 30%, transparent);
  }

  .btn-block {
    background: transparent;
    border-color: var(--color-hairline, #333);
    color: var(--color-dim, #888);
    font-size: 11px;
  }

  .btn-block:hover {
    background: color-mix(in srgb, var(--color-error, #e55) 8%, transparent);
    border-color: color-mix(in srgb, var(--color-error, #e55) 40%, transparent);
    color: var(--color-error, #e55);
  }

  .approval-hint {
    font-size: 11px;
    color: var(--color-dim, #777);
    line-height: 1.5;
    margin: 0;
    padding: 8px;
    background: var(--color-surface-soft, #222);
    border-radius: 8px;
  }

  @keyframes fadeIn {
    from { opacity: 0; }
    to { opacity: 1; }
  }

  @keyframes slideUp {
    from { transform: translateY(12px); opacity: 0; }
    to { transform: translateY(0); opacity: 1; }
  }
</style>
