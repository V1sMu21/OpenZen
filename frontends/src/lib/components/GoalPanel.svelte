<script lang="ts">
  import { onMount } from "svelte";
  import { t } from "../i18n";
  import { isTauri, tauriInvoke } from "../api/tauri";
  import { sessions } from "../stores/sessions";

  let { open = $bindable(false) } = $props();

  type Gate = { type: string; command?: string; prompt?: string };
  type Goal = {
    id: string;
    status: string;
    current_version: number;
    spec: {
      title: string;
      objective: string;
      session_id: string;
      gates: Gate[];
      budget: { max_attempts: number; max_wall_secs: number; max_tokens: number; max_turns_per_attempt: number; max_consecutive_failures: number; max_no_progress_rounds: number };
      auto_continue: boolean;
    };
    attempts: number;
    tokens_used: number;
  };

  let goals = $state<Goal[]>([]);
  let selected = $state<string | null>(null);
  let title = $state("");
  let objective = $state("");
  let gateCommand = $state("cargo test");
  let maxAttempts = $state(8);
  let autoContinue = $state(true);
  let requireHuman = $state(false);
  let autoStart = $state(true);
  let error = $state("");
  let busy = $state(false);

  onMount(() => {
    const select = (event: Event) => {
      const id = (event as CustomEvent<string>).detail;
      if (id) selected = id;
    };
    window.addEventListener("openzen:goal-select", select);
    return () => window.removeEventListener("openzen:goal-select", select);
  });

  const current = $derived(goals.find((g) => g.id === selected) ?? null);
  const statusLabel: Record<string, string> = {
    draft: "草稿", active: "进行中", paused: "已暂停", verifying: "验收中",
    blocked: "需要你", achieved: "已达成", exhausted: "预算耗尽", abandoned: "已放弃",
  };

  $effect(() => {
    if (open) void refresh();
  });

  async function refresh() {
    if (!isTauri()) { error = $t("steward.notTauri"); return; }
    try {
      goals = await tauriInvoke("goal_list");
      if (selected && !goals.some((g) => g.id === selected)) selected = null;
    } catch (e) { error = String(e); }
  }

  async function createGoal() {
    error = "";
    const sessionId = $sessions.currentId;
    if (!title.trim() || !objective.trim()) { error = "目标与描述不能为空"; return; }
    if (!sessionId) { error = "请先在左侧选择一个会话"; return; }
    busy = true;
    try {
      const gates = gateCommand.trim()
        ? [{ type: "command", command: gateCommand.trim(), timeout_secs: 120, retries: 0 }]
        : [];
      const created = await tauriInvoke("goal_create", {
        title: title.trim(),
        objective: objective.trim(),
        sessionId,
        gates,
        budget: {
          max_attempts: maxAttempts,
          max_wall_secs: 7200,
          max_tokens: 2_000_000,
          max_turns_per_attempt: 12,
          max_consecutive_failures: 3,
          max_no_progress_rounds: 2,
        },
        autoContinue,
        requireHumanAcceptance: requireHuman,
      });
      if (autoStart && created?.id) {
        await tauriInvoke("goal_start", { goalId: created.id, modelName: null });
      }
      title = ""; objective = "";
      await refresh();
    } catch (e) { error = String(e); } finally { busy = false; }
  }

  async function act(cmd: string, id: string) {
    error = ""; busy = true;
    try {
      if (cmd === "goal_start" || cmd === "goal_resume") {
        await tauriInvoke(cmd, { goalId: id, modelName: null });
      } else if (cmd === "goal_accept") {
        await tauriInvoke(cmd, { goalId: id });
      } else if (cmd === "goal_takeover") {
        const sid = await tauriInvoke(cmd, { goalId: id });
        if (sid) window.dispatchEvent(new CustomEvent("openzen:select-session", { detail: sid }));
      } else {
        await tauriInvoke(cmd, { goalId: id });
      }
      await refresh();
    } catch (e) { error = String(e); } finally { busy = false; }
  }

  function close() { open = false; }
</script>

{#if open}
  <div class="gp-overlay" role="button" tabindex="0" onclick={(e) => e.target === e.currentTarget && close()} onkeydown={(e) => e.key === "Escape" && close()}>
    <div class="gp-panel" role="dialog" aria-modal="true" aria-label="目标模式" tabindex="-1">
      <header class="gp-head">
        <div><div class="gp-kicker">GOAL MODE</div><h2>目标模式</h2></div>
        <button class="gp-icon" onclick={close} aria-label="关闭">×</button>
      </header>

      <div class="gp-body">
        <p class="gp-hint">发消息时自动判断长任务/验收循环并开启 Goal Mode；这里只用于手动覆盖或查看。</p>
        <section class="gp-create">
          <h3>新建目标</h3>
          <label>标题<input bind:value={title} placeholder="修好测试并提交" /></label>
          <label>目标<textarea bind:value={objective} rows="2" placeholder="验收标准是什么？"></textarea></label>
          <label>验收命令<input bind:value={gateCommand} placeholder="留空 → 人工验收" /></label>
          <div class="gp-row">
            <label>最多尝试<input type="number" min="1" max="50" bind:value={maxAttempts} /></label>
            <label class="gp-check"><input type="checkbox" bind:checked={autoContinue} /> 失败自动续跑</label>
            <label class="gp-check"><input type="checkbox" bind:checked={requireHuman} /> 必须人工验收</label>
            <label class="gp-check"><input type="checkbox" bind:checked={autoStart} /> 创建后立即开始</label>
          </div>
          <button class="gp-primary" onclick={createGoal} disabled={busy}>创建目标</button>
        </section>

        {#if error}<p class="gp-error">{error}</p>{/if}

        <section class="gp-list">
          <h3>目标列表</h3>
          {#if goals.length === 0}
            <p class="gp-empty">还没有目标。</p>
          {:else}
            {#each goals as g (g.id)}
              <button class="gp-card" class:selected={selected === g.id} onclick={() => (selected = g.id)}>
                <span class="gp-dot" class:on={g.status === "active"} class:bad={g.status === "blocked"}></span>
                <span class="gp-card-main">
                  <span class="gp-card-title">{g.spec.title}</span>
                  <span class="gp-card-meta">{statusLabel[g.status] ?? g.status} · attempt {g.attempts}/{g.spec.budget.max_attempts}</span>
                </span>
                <span class="gp-card-ver">{g.current_version}</span>
              </button>
            {/each}
          {/if}
        </section>

        {#if current}
          <section class="gp-detail">
            <h3>{current.spec.title}</h3>
            <p>{current.spec.objective}</p>
            <p class="gp-meta">
              {statusLabel[current.status] ?? current.status} · {current.spec.gates.length} gates · {current.tokens_used} tokens
            </p>
            <div class="gp-actions">
              {#if current.status === "draft" || current.status === "paused" || current.status === "blocked"}
                <button class="gp-btn" onclick={() => act(current.status === "draft" ? "goal_start" : "goal_resume", current.id)} disabled={busy}>开始 / 继续</button>
              {/if}
              {#if current.status === "active" || current.status === "verifying"}
                <button class="gp-btn" onclick={() => act("goal_pause", current.id)} disabled={busy}>暂停</button>
              {/if}
              {#if current.status === "verifying"}
                <button class="gp-primary" onclick={() => act("goal_accept", current.id)} disabled={busy}>人工验收</button>
              {/if}
              {#if current.status !== "achieved" && current.status !== "abandoned" && current.status !== "exhausted"}
                <button class="gp-btn" onclick={() => act("goal_stop", current.id)} disabled={busy}>停止</button>
              {/if}
              <button class="gp-btn" onclick={() => act("goal_takeover", current.id)}>接管</button>
            </div>
          </section>
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  .gp-overlay { position: fixed; inset: 0; z-index: 1000; background: rgba(4,6,8,.62); display: grid; place-items: center; padding: 24px; }
  .gp-panel { width: min(760px, 92vw); max-height: 86vh; overflow: auto; background: var(--color-surface, #11151a); border: 1px solid var(--color-hairline, #232a35); border-radius: 14px; box-shadow: 0 24px 80px rgba(0,0,0,.5); }
  .gp-head { display: flex; align-items: flex-start; justify-content: space-between; padding: 18px 20px 0; }
  .gp-kicker { font: 11px/1.2 var(--font-mono, monospace); letter-spacing: .14em; color: var(--color-gold, #cbb26a); }
  h2 { margin: 4px 0 0; color: var(--color-ink, #e8edf5); font-size: 22px; }
  h3 { margin: 0 0 8px; color: var(--color-ink, #e8edf5); font-size: 14px; }
  .gp-icon { border: 0; background: transparent; color: var(--color-muted, #8b96a6); font-size: 20px; cursor: pointer; }
  .gp-body { padding: 12px 20px 20px; display: grid; gap: 14px; }
  .gp-create, .gp-list, .gp-detail { border: 1px solid var(--color-hairline, #232a35); border-radius: 9px; padding: 12px; background: rgba(255,255,255,.015); }
  label { display: grid; gap: 4px; color: var(--color-muted, #8b96a6); font-size: 11px; margin: 6px 0; }
  input, textarea { width: 100%; box-sizing: border-box; background: #0a0c0f; border: 1px solid var(--color-hairline, #232a35); border-radius: 6px; color: var(--color-ink, #dce4ee); font: 12px/1.5 var(--font-mono, monospace); padding: 7px 8px; }
  .gp-row { display: flex; gap: 12px; align-items: end; flex-wrap: wrap; }
  .gp-row label { flex: 1; min-width: 110px; }
  .gp-check { display: flex; align-items: center; gap: 6px; font-size: 11px; white-space: nowrap; }
  .gp-check input { width: auto; }
  .gp-primary, .gp-btn { border: 1px solid var(--color-hairline, #232a35); border-radius: 6px; background: transparent; color: var(--color-ink, #dce4ee); font-size: 11px; padding: 7px 10px; cursor: pointer; }
  .gp-primary { background: var(--color-gold, #cbb26a); color: #11151a; border-color: transparent; font-weight: 600; }
  .gp-btn:hover { border-color: var(--color-gold, #cbb26a); }
  .gp-error { color: #e06c5b; font-size: 12px; margin: 0; }
  .gp-hint { margin: 0 2px 2px; color: var(--color-muted, #8b96a6); font-size: 11px; line-height: 1.6; }
  .gp-empty, .gp-meta { color: var(--color-muted, #8b96a6); font-size: 12px; margin: 4px 0; }
  .gp-card { display: flex; align-items: center; gap: 9px; width: 100%; text-align: left; border: 1px solid transparent; border-radius: 7px; background: transparent; color: inherit; padding: 8px; cursor: pointer; }
  .gp-card:hover, .gp-card.selected { border-color: var(--color-hairline, #232a35); background: rgba(203,178,106,.06); }
  .gp-dot { width: 7px; height: 7px; border-radius: 50%; background: #8b96a6; flex: none; }
  .gp-dot.on { background: #5fd3a3; }.gp-dot.bad { background: #d9a441; }
  .gp-card-main { display: grid; gap: 2px; flex: 1; }
  .gp-card-title { font-size: 13px; color: var(--color-ink, #e8edf5); }
  .gp-card-meta { font: 10px/1.4 var(--font-mono, monospace); color: var(--color-muted, #8b96a6); }
  .gp-card-ver { font: 10px var(--font-mono, monospace); color: var(--color-muted, #8b96a6); }
  .gp-actions { display: flex; gap: 6px; flex-wrap: wrap; margin-top: 10px; }
  @media (max-width: 640px) { .gp-overlay { padding: 8px; } .gp-panel { max-height: 94vh; } }
</style>
