<script lang="ts">
  import { onMount, tick } from "svelte";
  import { t } from "../i18n";
  import { isTauri, tauriInvoke } from "../api/tauri";

  let { open = $bindable(false) } = $props();

  type Snapshot = {
    id: string;
    title: string;
    status: string;
    phase: string;
    step_count: number;
    updated_at: number;
    last_verdict?: string | null;
  };
  type Step = {
    id: string;
    turn: number;
    phase: string;
    at: number;
    status: string;
    tool?: { name: string; status: string } | null;
  };
  type Status = {
    available: boolean;
    enabled: boolean;
    tickets?: number;
    inbox?: number;
    steps?: number;
    db_path?: string;
  };

  let status = $state<Status | null>(null);
  let lux = $state<{ acknowledged: boolean; path?: string } | null>(null);
  let rows = $state<Snapshot[]>([]);
  let selected = $state<string | null>(null);
  let steps = $state<Step[]>([]);
  let error = $state("");
  let loading = $state(false);
  let wasOpen = $state(false);
  let panelEl = $state<HTMLDivElement | undefined>();
  let previousFocus: HTMLElement | null = null;
  let inboxOnly = $state(false);

  async function refresh() {
    if (!isTauri()) {
      error = $t("steward.notTauri");
      return;
    }
    loading = true;
    error = "";
    try {
      status = await tauriInvoke("steward_status");
      lux = await tauriInvoke("steward_startlux_license_status");
      rows = await tauriInvoke(inboxOnly ? "steward_inbox" : "steward_snapshot", inboxOnly ? {} : { limit: 30 });
      if (selected && !rows.some((r) => r.id === selected)) {
        selected = null;
        steps = [];
      }
    } catch (e) {
      error = `${$t("steward.error")}: ${String(e)}`;
    } finally {
      loading = false;
    }
  }

  async function acknowledgeStartlux() {
    try {
      await tauriInvoke("steward_ack_startlux_license", { commercial: false });
      lux = await tauriInvoke("steward_startlux_license_status");
    } catch (e) {
      error = `${$t("steward.error")}: ${String(e)}`;
    }
  }

  async function toggleInbox() {
    inboxOnly = !inboxOnly;
    await refresh();
  }

  async function toggleEnabled() {
    if (!status?.available) {
      error = $t("steward.error");
      return;
    }
    try {
      const enabled = await tauriInvoke("steward_set_enabled", {
        enabled: !status.enabled,
      });
      status = { ...status, enabled };
    } catch (e) {
      error = `${$t("steward.error")}: ${String(e)}`;
    }
  }

  async function select(id: string) {
    if (selected === id) {
      selected = null;
      steps = [];
      return;
    }
    selected = id;
    try {
      steps = await tauriInvoke("steward_steps", { ticketId: id, limit: 40 });
    } catch (e) {
      error = `${$t("steward.error")}: ${String(e)}`;
    }
  }

  function close() {
    open = false;
    previousFocus?.focus();
    previousFocus = null;
  }

  function overlayClick(e: MouseEvent) {
    if ((e.target as HTMLElement).closest(".st-panel")) return;
    close();
  }

  function overlayKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      close();
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape" && open) close();
  }

  $effect(() => {
    if (open && !wasOpen) {
      wasOpen = true;
      previousFocus = document.activeElement as HTMLElement | null;
      void tick().then(() => panelEl?.focus());
      void refresh();
    } else if (!open) {
      wasOpen = false;
    }
  });

  onMount(() => {
    const handler = (e: KeyboardEvent) => onKeydown(e);
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  });

  function statusClass(value: string): string {
    if (value === "achieved") return "ok";
    if (value === "blocked" || value === "stalled" || value === "exhausted") return "warn";
    if (value === "failed" || value === "abandoned") return "bad";
    return "run";
  }

  function stamp(ms: number): string {
    try {
      return new Date(ms).toLocaleTimeString();
    } catch {
      return String(ms);
    }
  }
</script>

{#if open}
  <div class="st-overlay" role="button" tabindex="0" onclick={overlayClick} onkeydown={overlayKeydown}>
    <div
      class="st-panel"
      role="dialog"
      aria-modal="true"
      aria-label={$t("steward.title")}
      tabindex="-1"
      bind:this={panelEl}
    >
      <header class="st-head">
        <div>
          <div class="st-kicker">{$t("steward.status")}</div>
          <h2>{$t("steward.title")}</h2>
        </div>
        <button class="st-icon" onclick={close} aria-label={$t("steward.close")}>×</button>
      </header>

      <div class="st-toolbar">
        <span class="st-dot" class:on={status?.enabled}></span>
        <span class="st-state">
          {status?.enabled ? $t("steward.enable") : $t("steward.disabled")}
        </span>
        <span class="st-spacer"></span>
        <button class="st-btn" onclick={toggleEnabled} disabled={!status?.available}>
          {status?.enabled ? $t("steward.disable") : $t("steward.enable")}
        </button>
        <button class="st-btn" class:active={inboxOnly} onclick={() => void toggleInbox()} disabled={loading}>
          {$t("steward.inbox")}
        </button>
        <button class="st-btn" onclick={() => void refresh()} disabled={loading}>
          {$t("steward.refresh")}
        </button>
      </div>

      {#if status?.available}
        <div class="st-stats">
          <span><b>{status?.tickets ?? 0}</b> tickets</span>
          <span><b>{status?.steps ?? 0}</b> {$t("steward.steps")}</span>
          <span><b>{status?.inbox ?? 0}</b> {$t("steward.inbox")}</span>
        </div>
      {/if}

      <div class="st-license">
        <span class="st-license-name">{$t("steward.startlux")}</span>
        <span class="st-license-state" class:on={lux?.acknowledged}>
          {lux?.acknowledged ? $t("steward.licenseAcknowledged") : $t("steward.licensePending")}
        </span>
        {#if !lux?.acknowledged}
          <button class="st-btn" onclick={acknowledgeStartlux}>{$t("steward.licenseAck")}</button>
        {/if}
      </div>

      {#if error}
        <p class="st-error">{error}</p>
      {/if}

      <div class="st-list">
        {#if rows.length === 0}
          <p class="st-empty">{$t("steward.empty")}</p>
        {:else}
          {#each rows as row (row.id)}
            <button class="st-row" class:selected={selected === row.id} onclick={() => select(row.id)}>
              <span class="st-dot {statusClass(row.status)}"></span>
              <span class="st-main">
                <span class="st-title">{row.title}</span>
                <span class="st-meta">
                  {row.status} · {row.phase} · {row.step_count} {$t("steward.steps")} ·
                  {$t("steward.updated")} {stamp(row.updated_at)}
                </span>
              </span>
              <span class="st-verdict">{row.last_verdict ?? "—"}</span>
            </button>
          {/each}
        {/if}
      </div>

      {#if selected}
        <div class="st-details">
          <div class="st-details-head">{$t("steward.details")}</div>
          {#if steps.length === 0}
            <p class="st-empty">—</p>
          {:else}
            {#each steps as step (step.id)}
              <div class="st-step">
                <span class="st-step-turn">T{step.turn}</span>
                <span class="st-step-tool">{step.tool?.name ?? step.phase}</span>
                <span class="st-step-status" class:failed={step.status === "failed"}>
                  {step.status}
                </span>
                <span class="st-step-time">{stamp(step.at)}</span>
              </div>
            {/each}
          {/if}
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .st-overlay {
    position: fixed;
    inset: 0;
    z-index: 90;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 24px;
    background: color-mix(in srgb, var(--color-code-bg) 72%, transparent);
    backdrop-filter: blur(6px);
  }
  .st-panel {
    width: min(760px, 100%);
    max-height: min(760px, 88vh);
    overflow: auto;
    padding: 20px;
    border: 1px solid var(--color-hairline);
    border-radius: 12px;
    background: var(--color-surface);
  }
  .st-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
  }
  .st-kicker {
    font-family: var(--font-mono);
    font-size: 11px;
    letter-spacing: 0.14em;
    color: var(--color-ink);
    text-transform: uppercase;
  }
  h2 {
    margin: 4px 0 0;
    font-family: var(--font-serif, var(--font-sans));
    font-size: 24px;
    font-weight: 500;
    color: var(--color-ink);
  }
  .st-icon {
    width: 30px;
    height: 30px;
    border: 1px solid transparent;
    border-radius: 8px;
    background: none;
    color: var(--color-muted);
    font-size: 18px;
    cursor: pointer;
  }
  .st-icon:hover { color: var(--color-ink); background: var(--color-surface-soft); }
  .st-toolbar {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 14px;
    padding: 8px 10px;
    border: 1px solid var(--color-hairline);
    border-radius: 10px;
  }
  .st-state { font-size: 12px; color: var(--color-ink); }
  .st-spacer { flex: 1; }
  .st-btn {
    min-height: 28px;
    padding: 0 10px;
    border: 1px solid var(--color-hairline);
    border-radius: 8px;
    background: var(--color-surface-soft);
    color: var(--color-ink);
    font-size: 11px;
    cursor: pointer;
  }
  .st-btn:hover, .st-btn.active { border-color: var(--color-primary); color: var(--color-primary); }
  .st-btn:disabled { opacity: 0.45; cursor: not-allowed; }
  .st-dot {
    width: 8px;
    height: 8px;
    flex: 0 0 auto;
    border-radius: 50%;
    background: var(--color-muted);
  }
  .st-dot.on, .st-dot.ok { background: var(--color-success); }
  .st-dot.run { background: var(--color-primary); }
  .st-dot.warn { background: var(--color-status-warn); }
  .st-dot.bad { background: var(--color-status-danger); }
  .st-stats {
    display: flex;
    gap: 18px;
    margin-top: 12px;
    font-size: 11px;
    color: var(--color-ink);
  }
  .st-stats b { color: var(--color-ink); font-weight: 600; }
  .st-license {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 10px;
    padding: 8px 10px;
    border: 1px solid var(--color-hairline);
    border-radius: 8px;
    font-size: 11px;
  }
  .st-license-name { color: var(--color-ink); font-family: var(--font-mono); }
  .st-license-state { color: var(--color-status-warn); }
  .st-license-state.on { color: var(--color-success); }
  .st-error {
    margin-top: 10px;
    padding: 8px 10px;
    border: 1px solid var(--color-status-danger);
    border-radius: 8px;
    color: var(--color-status-danger);
    font-size: 12px;
  }
  .st-list {
    margin-top: 12px;
    border: 1px solid var(--color-hairline);
    border-radius: 10px;
    overflow: hidden;
  }
  .st-row {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 10px;
    border: 0;
    border-bottom: 1px solid var(--color-hairline);
    background: none;
    color: inherit;
    text-align: left;
    cursor: pointer;
  }
  .st-row:last-child { border-bottom: 0; }
  .st-row:hover, .st-row.selected { background: var(--color-surface-soft); }
  .st-main { display: flex; flex-direction: column; gap: 2px; min-width: 0; flex: 1; }
  .st-title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12px;
    color: var(--color-ink);
  }
  .st-meta { font-size: 11px; color: var(--color-ink); }
  .st-verdict {
    flex: 0 0 auto;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--color-ink);
  }
  .st-empty { padding: 16px 12px; margin: 0; font-size: 12px; color: var(--color-ink); }
  .st-details { margin-top: 12px; }
  .st-details-head {
    font-family: var(--font-mono);
    font-size: 11px;
    letter-spacing: 0.12em;
    color: var(--color-ink);
    text-transform: uppercase;
  }
  .st-step {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 8px;
    border-bottom: 1px solid var(--color-hairline);
    font-size: 11px;
  }
  .st-step-turn { width: 32px; color: var(--color-ink); font-family: var(--font-mono); }
  .st-step-tool { flex: 1; color: var(--color-ink); }
  .st-step-status { color: var(--color-success); }
  .st-step-status.failed { color: var(--color-status-danger); }
  .st-step-time { color: var(--color-ink); font-family: var(--font-mono); font-size: 11px; }
</style>
