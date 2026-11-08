<script lang="ts">
  import { onMount } from "svelte";
  import { sessions } from "../stores/sessions";
  import { isTauri, tauriInvoke } from "../api/tauri";

  type Goal = {
    id: string;
    status: string;
    attempts: number;
    spec: {
      title: string;
      objective: string;
      session_id: string;
      gates: unknown[];
      budget: { max_attempts: number };
      auto_continue: boolean;
    };
  };

  let goal = $state<Goal | null>(null);
  let timer: ReturnType<typeof setInterval> | undefined;

  const labels: Record<string, string> = {
    draft: "草稿", active: "进行中", paused: "已暂停", verifying: "验收中",
    blocked: "需要你", achieved: "已达成", exhausted: "预算耗尽", abandoned: "已放弃",
  };

  async function refresh() {
    if (!isTauri()) return;
    const sid = $sessions.currentId;
    if (!sid) { goal = null; return; }
    try {
      const goals: Goal[] = await tauriInvoke("goal_list");
      goal = goals.find((g) => g.spec.session_id === sid) ?? null;
    } catch {
      // Goal banner must never break the chat surface.
    }
  }

  function openGoals() {
    if (goal) {
      window.dispatchEvent(new CustomEvent("openzen:goal-select", { detail: goal.id }));
    }
    window.dispatchEvent(new CustomEvent("openzen:open-goals"));
  }

  $effect(() => {
    // re-query whenever the active session changes
    const _sid = $sessions.currentId;
    void refresh();
  });

  onMount(() => {
    timer = setInterval(() => void refresh(), 5000);
    const onRefresh = () => void refresh();
    window.addEventListener("openzen:goal-refresh", onRefresh);
    return () => {
      if (timer) clearInterval(timer);
      window.removeEventListener("openzen:goal-refresh", onRefresh);
    };
  });
</script>

{#if goal && goal.status !== "abandoned"}
  <div class="goal-banner" class:verify={goal.status === "verifying"} class:done={goal.status === "achieved"} role="status" aria-live="polite">
    <span class="gb-kicker">GOAL</span>
    <span class="gb-dot" class:on={goal.status === "active"} class:bad={goal.status === "blocked" || goal.status === "verifying"} class:ok={goal.status === "achieved"}></span>
    <span class="gb-main">
      <span class="gb-title">{goal.spec.title}</span>
      <span class="gb-meta">
        {labels[goal.status] ?? goal.status}
        · attempt {goal.attempts}/{goal.spec.budget.max_attempts}
        · {goal.spec.gates.length} gates
        {#if goal.status === "verifying"} · 等待人工验收{/if}
      </span>
    </span>
    <button class="gb-btn" onclick={openGoals}>{goal.status === "verifying" ? "去验收" : "查看目标"}</button>
  </div>
{/if}

<style>
  .goal-banner {
    display: flex;
    align-items: center;
    gap: 9px;
    margin: 10px 18px 0;
    padding: 7px 10px;
    border: 1px solid var(--color-hairline, #232a35);
    border-left: 2px solid var(--color-gold, #cbb26a);
    border-radius: 7px;
    background: rgba(203, 178, 106, .045);
    font-size: 12px;
    color: var(--color-ink, #c3cbd8);
  }
  .goal-banner.verify { border-left-color: var(--color-status-warn, #d9a441); background: rgba(217, 164, 65, .06); }
  .goal-banner.done { border-left-color: var(--color-success, #5fd3a3); }
  .gb-kicker { font: 9px/1 var(--font-mono, monospace); letter-spacing: .16em; color: var(--color-gold, #cbb26a); flex: none; }
  .gb-dot { width: 6px; height: 6px; border-radius: 50%; background: var(--color-muted, #8b96a6); flex: none; }
  .gb-dot.on { background: var(--color-success, #5fd3a3); }
  .gb-dot.bad { background: var(--color-status-warn, #d9a441); }
  .gb-dot.ok { background: var(--color-success, #5fd3a3); }
  .gb-main { display: flex; align-items: baseline; gap: 8px; min-width: 0; flex: 1; }
  .gb-title { color: var(--color-ink, #e8edf5); font-size: 12px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; max-width: 46%; }
  .gb-meta { color: var(--color-muted, #8b96a6); font: 10px/1.4 var(--font-mono, monospace); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .gb-btn { flex: none; border: 1px solid var(--color-hairline, #232a35); border-radius: 5px; background: transparent; color: var(--color-ink, #dce4ee); font-size: 10px; padding: 4px 8px; cursor: pointer; }
  .gb-btn:hover { border-color: var(--color-gold, #cbb26a); }
  @media (max-width: 720px) { .gb-title { max-width: 35%; } .gb-meta { display: none; } }
</style>
