<script lang="ts">
  import { chat } from "../stores/chat";
  import { sessions } from "../stores/sessions";
  import { t } from "../i18n";
  import { getAuthToken } from "../api/chat";

  type AgentEntry = {
    name: string;
    model: string;
    tools: string[];
    has_instructions: boolean;
  };

  let { open = $bindable(false), onSelect = (name: string) => {} } = $props();
  let agents = $state<AgentEntry[]>([]);
  let loading = $state(true);
  let selectedName = $state<string | null>(null);

  $effect(() => {
    if (!open) return;
    loading = true;
    const headers: Record<string, string> = {};
    const token = getAuthToken();
    if (token) headers["Authorization"] = `Bearer ${token}`;
    fetch("/api/agents", { headers })
      .then((r) => r.json())
      .then((data) => { agents = data; loading = false; })
      .catch(() => { loading = false; });
  });

  function select(name: string) {
    selectedName = name;
    onSelect(name);
    open = false;
  }
</script>

{#if open}
  <div class="overlay" onclick={() => (open = false)} onkeydown={(e) => e.key === "Escape" && (open = false)} role="dialog">
    <div class="picker" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.stopPropagation()}>
      <div class="picker-header">
        <h3>Select Agent</h3>
        <button class="close-btn" onclick={() => (open = false)} aria-label={$t("model.close")}>
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
            <path d="M4 4l8 8M12 4l-8 8" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
          </svg>
        </button>
      </div>
      {#if loading}
        <div class="loading">{$t("agent.loading")}</div>
      {:else if agents.length === 0}
        <div class="empty">No agents found. Create one at ~/.openzen/agents/&lt;name&gt;/config.yaml</div>
      {:else}
        <div class="agent-list">
          {#each agents as agent (agent.name)}
            <button class="agent-card" class:selected={selectedName === agent.name} onclick={() => select(agent.name)}>
              <div class="agent-name">{agent.name}</div>
              <div class="agent-meta">
                <span class="agent-model">{agent.model || "default"}</span>
                {#if agent.tools.length > 0}
                  <span class="agent-tools">{agent.tools.length} {$t("message.tools")}</span>
                {/if}
                {#if agent.has_instructions}
                  <span class="agent-instructions">has instructions</span>
                {/if}
              </div>
            </button>
          {/each}
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
    backdrop-filter: blur(4px);
  }
  .picker {
    background: var(--color-surface);
    border: 1px solid var(--color-hairline-strong);
    border-radius: 16px;
    width: min(400px, 90vw);
    max-height: 60vh;
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }
  .picker-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 16px 20px 12px;
    border-bottom: 1px solid var(--color-hairline);
  }
  .picker-header h3 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
  }
  .close-btn {
    background: none;
    border: none;
    color: var(--color-dim);
    cursor: pointer;
    padding: 4px;
    border-radius: 4px;
  }
  .close-btn:hover { color: var(--color-ink); }
  .loading, .empty {
    padding: 24px 20px;
    text-align: center;
    color: var(--color-muted);
    font-size: 14px;
  }
  .agent-list {
    overflow-y: auto;
    padding: 8px;
  }
  .agent-card {
    display: block;
    width: 100%;
    text-align: left;
    background: none;
    border: 1px solid transparent;
    border-radius: 10px;
    padding: 10px 14px;
    cursor: pointer;
    color: var(--color-ink);
    margin-bottom: 4px;
    transition: background 0.1s, border 0.1s;
  }
  .agent-card:hover { background: var(--color-surface-elevated); }
  .agent-card.selected { border-color: var(--color-primary); background: var(--color-primary-muted); }
  .agent-name { font-size: 14px; font-weight: 600; margin-bottom: 4px; }
  .agent-meta { display: flex; gap: 8px; flex-wrap: wrap; }
  .agent-model {
    font-size: 11px;
    color: var(--color-body);
    background: var(--color-surface-soft);
    padding: 1px 8px;
    border-radius: 4px;
  }
  .agent-tools, .agent-instructions {
    font-size: 11px;
    color: var(--color-dim);
    background: var(--color-surface-soft);
    padding: 1px 8px;
    border-radius: 4px;
  }
</style>
