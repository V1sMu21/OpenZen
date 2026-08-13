<script lang="ts">
  import type { DataPart } from "../stores/parts";

  let { parts = [] as DataPart[] } = $props();

  let visible = $derived(parts.filter((p) => p.transient));
  let queued = $state<DataPart[]>([]);
  let current = $state<DataPart | null>(null);
  let shownIds = new Set<string>();

  // Enqueue new transient parts. This effect re-runs whenever `parts`
  // changes (the parent passes a freshly-filtered array), so it must stay
  // cheap and side-effect-light: it only appends to the queue.
  $effect(() => {
    const v = visible;
    for (const p of v) {
      if (shownIds.has(p.id)) continue;
      if (!queued.some((q) => q.id === p.id) && current?.id !== p.id) {
        queued = [...queued, p];
      }
    }
    if (!current && queued.length > 0) {
      current = queued[0];
      queued = queued.slice(1);
      shownIds.add(current.id);
      // Bound memory: transient ids are per-session and short-lived.
      if (shownIds.size > 500) {
        const oldest = shownIds.values().next();
        if (oldest.value !== undefined) shownIds.delete(oldest.value);
      }
    }
  });

  // Auto-dismiss. The 4s timeout lives in its own effect keyed only on the
  // current part's id — the enqueue effect re-runs on every parts change
  // and used to clear this very timeout in its cleanup, so the bar got
  // stuck on the first notification forever.
  $effect(() => {
    const id = current?.id;
    if (!id) return;
    const t = setTimeout(() => {
      if (current?.id === id) current = null;
    }, 4000);
    return () => clearTimeout(t);
  });
</script>

{#if current}
  <div class="transients-bar">
    <span class="transient-message">{current.content}</span>
    <div class="transient-progress"></div>
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
