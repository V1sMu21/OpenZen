<script lang="ts">
  import type { DataPart, UIMessagePart } from "../stores/parts";

  // Only the newest in-progress notice is shown. The old implementation
  // queued every notice (including one per LLM retry: 8 × 60s = 8 minutes of
  // stale "模型暂无响应" after the run had already stopped or recovered).
  // A notice is also dropped the moment anything newer streams in — the model
  // answered, so the retry banner must not linger.
  let { parts = [] as UIMessagePart[] } = $props();

  let latest = $derived.by<DataPart | null>(() => {
    let idx = -1;
    for (let i = parts.length - 1; i >= 0; i--) {
      const p = parts[i];
      if (p.type === "data" && p.transient) {
        idx = i;
        break;
      }
    }
    if (idx < 0) return null;
    // Anything after the notice means the run moved on.
    if (idx < parts.length - 1) return null;
    return parts[idx] as DataPart;
  });

  // Dismissal is per-id: when a newer notice replaces the current one the
  // timer is re-armed, and a late timer from an older id is ignored.
  let dismissedId = $state<string | null>(null);
  let current = $derived(latest && latest.id !== dismissedId ? latest : null);

  let ttl = $derived(
    current?.dataType === "data_quality_gate"
      ? 15000
      : current?.dataType === "llm_retry"
        ? 20000
        : 4000,
  );

  $effect(() => {
    const id = current?.id;
    if (!id) return;
    const t = setTimeout(() => {
      if (latest?.id === id) dismissedId = id;
    }, ttl);
    return () => clearTimeout(t);
  });
</script>

{#if current}
  <div class="transients-bar" role="status" aria-live="polite">
    <span class="transient-message">{current.content}</span>
    <!-- Duration must match the auto-dismiss timer, otherwise the bar lies. -->
    <div class="transient-progress" style="animation-duration:{ttl}ms"></div>
  </div>
{/if}

<style>
  .transients-bar {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    background: var(--color-surface-overlay);
    border-bottom: 1px solid var(--color-hairline-strong);
    padding: 6px 16px;
    font-size: 12px;
    color: var(--color-body);
    z-index: 50;
    animation: slideDown 0.2s ease-out;
  }
  .transient-message {
    opacity: 0.85;
  }
  .transient-progress {
    height: 2px;
    background: var(--color-primary);
    margin-top: 4px;
    border-radius: 1px;
    /* Duration comes from the component (`animation-duration` inline) so the
       bar always matches the TTL; 4s here is only a fallback. */
    animation: shrinkBar 4s linear;
    transform-origin: left;
  }
  @keyframes slideDown {
    from { transform: translateY(-100%); opacity: 0; }
    to { transform: translateY(0); opacity: 1; }
  }
  @keyframes shrinkBar {
    from { transform: scaleX(0); }
    to { transform: scaleX(1); }
  }
</style>
