<script lang="ts">
  import type { ModelInfo } from "../stores/types";
  import { formatTokenCount } from "../stores/types";
  import { localT } from "../i18n/index";

  let { modelInfo = $bindable<ModelInfo | null>(null), contextUsed = $bindable(0) } = $props();
</script>

{#if modelInfo}
  <div class="model-badge">
    <div class="badge-row">
      <span class="model-name" title={modelInfo.model}>
        {modelInfo.model}
      </span>
      <span class="provider-badge" class:local={modelInfo.isLocal} class:online={!modelInfo.isLocal}>
        {modelInfo.isLocal ? localT("status.localDeploy", "Local") : localT("status.cloud", "Cloud")}
      </span>
    </div>
    {#if modelInfo.contextWindow > 0}
      <div class="context-bar">
        <div class="context-track">
          <div
            class="context-fill"
            style="width: {Math.min((contextUsed / modelInfo.contextWindow) * 100, 100)}%"
          ></div>
        </div>
        <span class="context-label">
          {formatTokenCount(contextUsed)} / {formatTokenCount(modelInfo.contextWindow)}
        </span>
      </div>
    {/if}
  </div>
{/if}

<style>
  .model-badge {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin: 0 0 6px 0;
  }

  .badge-row {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .model-name {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--color-dim);
    line-height: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .provider-badge {
    font-size: 9px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    padding: 1px 6px;
    border-radius: 4px;
    line-height: 16px;
    flex-shrink: 0;
  }

  .provider-badge.online {
    background: color-mix(in srgb, var(--color-accent) 15%, transparent);
    color: var(--color-accent);
  }

  .provider-badge.local {
    background: color-mix(in srgb, var(--color-info) 15%, transparent);
    color: var(--color-info);
  }

  .context-bar {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .context-track {
    flex: 1;
    height: 3px;
    background: var(--color-hairline);
    border-radius: 2px;
    overflow: hidden;
  }

  .context-fill {
    height: 100%;
    background: var(--color-accent);
    border-radius: 2px;
    transition: width 0.3s ease;
  }

  .context-label {
    font-size: 10px;
    color: var(--color-dim);
    font-family: var(--font-mono);
    white-space: nowrap;
    flex-shrink: 0;
  }
</style>
