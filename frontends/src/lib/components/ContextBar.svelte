<script lang="ts">
  import { chat } from "../stores/chat";
  import { t } from "../i18n";

  let modelInfo = $state<{ contextWindow: number } | null>(null);
  let contextUsed = $state(0);

  $effect(() => {
    const unsub = chat.subscribe((s) => {
      // chat fires on every stream token — only touch state when the values
      // actually change, so the bar doesn't re-render per token and doesn't
      // churn a fresh modelInfo object each update.
      const cw = s.modelInfo?.contextWindow ?? null;
      if (cw !== (modelInfo?.contextWindow ?? null)) {
        modelInfo = cw != null ? { contextWindow: cw } : null;
      }
      const last = s.messages[s.messages.length - 1];
      const used = last?.contextTokens ?? 0;
      if (used !== contextUsed) {
        contextUsed = used;
      }
    });
    return unsub;
  });

  let contextWindow = $derived(modelInfo?.contextWindow ?? 128_000);
  let pct = $derived(contextWindow > 0 ? Math.min(100, Math.max(0, (contextUsed / contextWindow) * 100)) : 0);
  let barColor = $derived(
    pct < 70 ? "var(--success, #5db872)" :
    pct < 90 ? "var(--warning, #d4a017)" :
    "var(--error, #c64545)"
  );
</script>

<div class="context-bar">
  <span class="context-label">{$t("context.label")}</span>
  <div class="context-bar-track">
    <div class="context-bar-fill" style="width:{pct}%; background:{barColor};"></div>
  </div>
  <span class="context-pct" style="color:{barColor}">{pct.toFixed(0)}%</span>
</div>

<style>
  .context-bar {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 2px 16px 2px;
    border-top: 1px solid var(--color-hairline);
    background: var(--color-canvas);
    flex-shrink: 0;
  }

  .context-label {
    font-size: 9px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--color-muted);
    flex-shrink: 0;
  }

  .context-bar-track {
    flex: 1;
    height: 3px;
    border-radius: 2px;
    background: var(--color-surface-soft);
    overflow: hidden;
    min-width: 24px;
  }

  .context-bar-fill {
    height: 100%;
    border-radius: 2px;
    transition: width 0.4s ease, background 0.4s ease;
  }

  .context-pct {
    font-size: 10px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    flex-shrink: 0;
    min-width: 22px;
    text-align: right;
  }
</style>
