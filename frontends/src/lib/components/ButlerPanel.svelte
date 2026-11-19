<script lang="ts">
  import { onMount } from "svelte";
  import { t } from "../i18n";
  import { isTauri, tauriInvoke } from "../api/tauri";
  import { sessions } from "../stores/sessions";

  let { open = $bindable(false) } = $props();

  type Task = {
    id: string;
    goal: string;
    status: string;
    attempt: number;
    max_attempts: number;
    deps: string[];
    assumptions: string[];
    skipped: string[];
    project_name?: string | null;
    project_id: string;
    detail?: string;
    accept?: unknown[];
    evidence?: unknown[];
    owner_session?: string | null;
    created_at?: number;
    updated_at?: number;
  };
  type TaskEvent = { id: string; task_id: string; at: number; kind: string; detail: string };
  type Approval = {
    id: string;
    task_id: string;
    action: string;
    impact: string;
    reversibility: string;
  };
  type Project = { id: string; name: string; root_path: string };
  type Run = {
    run_id: string;
    task_id: string;
    project_id: string;
    child_id: string;
    model: string;
    started_at: number;
    finished_at?: number | null;
    status: string;
    summary: string;
  };
  type RunDetail = { run: Run; messages: unknown[] };
  type Automation = {
    id?: string;
    kind: string;
    message: string;
    session_id: string;
    fire_at_ms: number;
    repeat_count: number;
    repeat_interval_secs: number;
  };
  type Status = {
    available: boolean;
    paused?: boolean;
    tasks?: number;
    open?: number;
    verified?: number;
    unverified?: number;
    failed?: number;
    pending_approvals?: number;
  };

  let status = $state<Status | null>(null);
  let tasks = $state<Task[]>([]);
  let approvals = $state<Approval[]>([]);
  let projects = $state<Project[]>([]);
  let runs = $state<Run[]>([]);
  let automations = $state<Automation[]>([]);
  let goals = $state<{ total: number; active: number }>({ total: 0, active: 0 });
  let steward = $state<{ open: number; total: number }>({ open: 0, total: 0 });
  let platforms = $state<{ name: string; enabled: boolean }[]>([]);
  let runDetail = $state<RunDetail | null>(null);
  let runLoading = $state(false);
  let selectedTaskId = $state<string | null>(null);
  let taskEvents = $state<TaskEvent[]>([]);
  let taskLoading = $state(false);
  let tab = $state<"projects" | "tasks" | "runs" | "automations" | "approvals">("projects");
  let error = $state("");
  let loading = $state(false);
  let runSeq = 0;

  async function refresh() {
    if (!isTauri()) {
      error = $t("butler.notTauri");
      return;
    }
    loading = true;
    // Each panel is independent: one failing command must not turn the other
    // tabs into a fake empty state (which looked like "no projects/runs").
    const [st, ts, ap, pr, ru, au] = await Promise.allSettled([
      tauriInvoke("butler_status"),
      tauriInvoke("butler_tasks", { projectId: null }),
      tauriInvoke("butler_approvals"),
      tauriInvoke("list_projects"),
      tauriInvoke("butler_runs", { projectId: null, limit: 40 }),
      tauriInvoke("butler_automations"),
    ]);
    const errors: string[] = [];
    if (st.status === "fulfilled") status = st.value;
    else errors.push(String(st.reason));
    if (ts.status === "fulfilled") tasks = ts.value ?? [];
    else errors.push(String(ts.reason));
    if (ap.status === "fulfilled") approvals = ap.value ?? [];
    else errors.push(String(ap.reason));
    if (pr.status === "fulfilled") projects = pr.value ?? [];
    else errors.push(String(pr.reason));
    if (ru.status === "fulfilled") runs = ru.value ?? [];
    else errors.push(String(ru.reason));
    if (au.status === "fulfilled") {
      automations = au.value?.reminders ?? [];
      goals = au.value?.goals ?? { total: 0, active: 0 };
      steward = au.value?.steward ?? { open: 0, total: 0 };
      platforms = au.value?.platforms ?? [];
    } else errors.push(String(au.reason));
    error = errors.length ? `${$t("butler.error")}: ${errors[0]}` : "";
    if (selectedTaskId) {
      const want = selectedTaskId;
      try {
        const events = (await tauriInvoke("butler_task_events", { taskId: want })) ?? [];
        if (selectedTaskId === want) taskEvents = events;
      } catch {
        // Keep the last timeline; the top-level error already reports it.
      }
    }
    loading = false;
  }

  async function openProjectSession(projectId: string | null) {
    try {
      const res = await tauriInvoke("butler_ensure_session", { projectId });
      if (res?.session_id) {
        await sessions.load();
        window.dispatchEvent(
          new CustomEvent("openzen:select-session", { detail: res.session_id }),
        );
      }
      close();
    } catch (e) {
      error = `${$t("butler.error")}: ${String(e)}`;
    }
  }

  async function reveal(path: string) {
    try {
      await tauriInvoke("reveal_in_finder", { path });
    } catch (e) {
      error = `${$t("butler.error")}: ${String(e)}`;
    }
  }

  async function cancelReminder(index: number, id?: string) {
    try {
      await tauriInvoke("butler_cancel_reminder", { index, id: id ?? null });
      await refresh();
    } catch (e) {
      error = `${$t("butler.error")}: ${String(e)}`;
    }
  }

  async function openTask(task: Task) {
    if (selectedTaskId === task.id) {
      selectedTaskId = null;
      return;
    }
    selectedTaskId = task.id;
    taskLoading = true;
    taskEvents = [];
    const want = task.id;
    try {
      const events = (await tauriInvoke("butler_task_events", { taskId: task.id })) ?? [];
      if (selectedTaskId === want) taskEvents = events;
    } catch (e) {
      if (selectedTaskId === want) error = `${$t("butler.error")}: ${String(e)}`;
    } finally {
      if (selectedTaskId === want) taskLoading = false;
    }
  }

  function taskStatusText(task: Task): string {
    const base = label[task.status] ?? task.status;
    if (task.status === "queued" || task.status === "ready") {
      const deps = task.deps
        .map((id) => tasks.find((t) => t.id === id))
        .filter(Boolean) as Task[];
      if (deps.length) {
        const pending = deps.filter((d) => !["verified", "unverified"].includes(d.status));
        if (pending.length) return `${base} · 等待 ${pending.map((d) => `「${d.goal.slice(0, 18)}」`).join("、")}`;
        return `${base} · 依赖已结束（含未验证），即将派工`;
      }
      return `${base} · 待调度`;
    }
    if (task.status === "running") return `${base} · 工人执行中`;
    if (task.status === "verifying") return `${base} · 独立验收中`;
    if (task.status === "unverified") return base;
    if (task.status === "blocked") return `${base} · 依赖失败/待人工处理`;
    return base;
  }

  function taskRun(taskId: string): Run | undefined {
    return runs.find((r) => r.task_id === taskId);
  }

  async function openRun(run: Run) {
    const seq = ++runSeq;
    runDetail = null;
    runLoading = true;
    try {
      const detail = await tauriInvoke("butler_run_detail", { runId: run.run_id });
      if (seq === runSeq) runDetail = detail;
    } catch (e) {
      if (seq === runSeq) error = `${$t("butler.error")}: ${String(e)}`;
    } finally {
      if (seq === runSeq) runLoading = false;
    }
  }

  function msgText(m: unknown): string {
    if (typeof m === "string") return m;
    if (!m || typeof m !== "object") return "";
    const o = m as Record<string, unknown>;
    if (typeof o.content === "string") return o.content;
    if (Array.isArray(o.content)) {
      return o.content
        .map((b) => {
          if (!b || typeof b !== "object") return "";
          const bi = b as Record<string, unknown>;
          if (bi.type === "text") return String(bi.text ?? "");
          if (bi.type === "tool_use")
            return `→ ${String(bi.name ?? "tool")} ${JSON.stringify(bi.input ?? {}).slice(0, 180)}`;
          if (bi.type === "tool_result")
            return `← ${String(bi.content ?? "").slice(0, 180)}`;
          return "";
        })
        .filter(Boolean)
        .join("\n");
    }
    return "";
  }

  async function decide(id: string, approved: boolean) {
    try {
      await tauriInvoke("butler_decide", { approvalId: id, approved });
      await refresh();
    } catch (e) {
      error = `${$t("butler.error")}: ${String(e)}`;
    }
  }

  async function togglePause() {
    try {
      await tauriInvoke("butler_pause", { paused: !status?.paused });
      await refresh();
    } catch (e) {
      error = `${$t("butler.error")}: ${String(e)}`;
    }
  }

  function close() {
    open = false;
  }

  function projectName(projectId: string): string {
    return projects.find((p) => p.root_path === projectId)?.name ?? projectId.split("/").pop() ?? "workspace";
  }

  function when(ms: number): string {
    if (!ms) return "";
    return new Date(ms).toLocaleString();
  }

  function duration(run: Run): string {
    if (!run.finished_at) return "…";
    const s = Math.max(0, Math.round((run.finished_at - run.started_at) / 1000));
    return s >= 60 ? `${Math.floor(s / 60)}m${s % 60}s` : `${s}s`;
  }

  onMount(() => {
    void refresh();
  });

  $effect(() => {
    if (!open) return;
    void refresh();
    const id = setInterval(() => void refresh(), 5000);
    return () => clearInterval(id);
  });

  const label: Record<string, string> = {
    queued: "排队中",
    ready: "待派工",
    running: "执行中",
    verifying: "核验中",
    verified: "已完成 · 已验证",
    unverified: "已完成 · 证据不足",
    blocked: "待批准/阻塞",
    failed: "失败",
  };
</script>

{#if open}
  <div
    class="bt-overlay"
    role="button"
    tabindex="0"
    onclick={(e) => e.target === e.currentTarget && close()}
    onkeydown={(e) => e.key === "Escape" && close()}
  >
    <div class="bt-panel" role="dialog" aria-modal="true" aria-label={$t("butler.title")}>
      <header class="bt-head">
        <div>
          <div class="bt-kicker">{$t("butler.status")}</div>
          <h2>{$t("butler.title")}</h2>
        </div>
        <button class="bt-icon" onclick={close} aria-label={$t("butler.close")}>×</button>
      </header>

      <div class="bt-toolbar">
        <span
          class="bt-dot"
          class:on={status?.available && !status?.paused && (status?.open ?? 0) > 0}
        ></span>
        <span class="bt-state">
          {status?.available
            ? status?.paused
              ? $t("butler.paused")
              : (status?.open ?? 0) > 0
                ? $t("butler.running")
                : $t("butler.idle")
            : $t("butler.unavailable")}
        </span>
        <span class="bt-spacer"></span>
        <button class="bt-btn" onclick={() => void refresh()} disabled={loading}>
          {$t("butler.refresh")}
        </button>
        <button class="bt-btn" onclick={() => void togglePause()} disabled={!status?.available}>
          {status?.paused ? $t("butler.resume") : $t("butler.pause")}
        </button>
        <button class="bt-primary" onclick={() => void openProjectSession(null)}>
          {$t("butler.openSession")}
        </button>
      </div>

      {#if error}<p class="bt-error">{error}</p>{/if}

      <div class="bt-stats">
        <span><b>{status?.tasks ?? 0}</b> {$t("butler.tasks")}</span>
        <span><b>{status?.verified ?? 0}</b> {$t("butler.verified")}</span>
        <span><b>{status?.unverified ?? 0}</b> {$t("butler.unverified")}</span>
        <span><b>{status?.failed ?? 0}</b> {$t("butler.failed")}</span>
        <span><b>{status?.pending_approvals ?? 0}</b> {$t("butler.pending")}</span>
      </div>

      <nav class="bt-tabs">
        <button class:active={tab === "projects"} onclick={() => (tab = "projects")}>{$t("butler.tab.projects")}</button>
        <button class:active={tab === "tasks"} onclick={() => (tab = "tasks")}>{$t("butler.tab.tasks")}</button>
        <button class:active={tab === "runs"} onclick={() => (tab = "runs")}>{$t("butler.tab.runs")}</button>
        <button class:active={tab === "automations"} onclick={() => (tab = "automations")}>{$t("butler.tab.automations")}</button>
        <button class:active={tab === "approvals"} onclick={() => (tab = "approvals")}>
          {$t("butler.tab.approvals")} {#if approvals.length}<em>{approvals.length}</em>{/if}
        </button>
      </nav>

      {#if tab === "projects"}
        <section class="bt-sec">
          {#if projects.length === 0}
            <p class="bt-empty">{$t("butler.projects.empty")}</p>
          {:else}
            {#each projects as p (p.id)}
              <div class="bt-project">
                <div class="bt-project-main">
                  <b>{p.name}</b>
                  <small>{p.root_path}</small>
                  <small>
                    {tasks.filter((t) => t.project_id === p.root_path).length} {$t("butler.tasks")} ·
                    {runs.filter((r) => r.project_id === p.root_path).length} {$t("butler.runs")}
                  </small>
                </div>
                <div class="bt-project-actions">
                  <button class="bt-btn" onclick={() => void reveal(p.root_path)}>Finder</button>
                  <button class="bt-btn" onclick={() => void openProjectSession(p.id)}>{$t("butler.project.openChat")}</button>
                </div>
              </div>
            {/each}
          {/if}
        </section>
      {:else if tab === "tasks"}
        <section class="bt-sec">
          {#if tasks.length === 0}
            <p class="bt-empty">{$t("butler.empty")}</p>
          {:else}
            {#each tasks as task (task.id)}
              <div class="bt-task" role="button" tabindex="0" onclick={() => void openTask(task)} onkeydown={(e) => e.target === e.currentTarget && e.key === "Enter" && void openTask(task)}>
                <span class="bt-pill {task.status}">{label[task.status] ?? task.status}</span>
                <div class="bt-task-main">
                  <b>{task.goal}</b>
                  <small>
                    {#if task.project_name}<span class="bt-proj">{task.project_name}</span>{/if}
                    {task.attempt}/{task.max_attempts}
                    {#if task.updated_at}· {when(task.updated_at)}{/if}
                  </small>
                  <small>{taskStatusText(task)}</small>
                  {#if selectedTaskId === task.id}
                    <div class="bt-task-detail">
                      {#if task.detail}<p>{task.detail}</p>{/if}
                      {#if task.evidence?.length}
                        <small>{$t("butler.evidence")}: {task.evidence.length} {$t("butler.items")}</small>
                      {/if}
                      {#if taskLoading}
                        <small>{$t("butler.events.loading")}</small>
                      {:else if taskEvents.length}
                        <div class="bt-events">
                          {#each taskEvents as ev (ev.id)}
                            <div><span>{when(ev.at)}</span> · <b>{ev.kind}</b> · {ev.detail}</div>
                          {/each}
                        </div>
                      {:else}
                        <small>{$t("butler.events.empty")}</small>
                      {/if}
                      {#if taskRun(task.id)}
                        <button class="bt-btn" onclick={(e) => { e.stopPropagation(); void openRun(taskRun(task.id)!); }}>
                          {$t("butler.task.viewRun")}
                        </button>
                      {/if}
                    </div>
                  {/if}
                </div>
              </div>
            {/each}
          {/if}
        </section>
      {:else if tab === "runs"}
        <section class="bt-sec">
          {#if runLoading && !runDetail}<p>{$t("butler.runs.loading")}</p>{/if}
          {#if runDetail}
            <button class="bt-btn" onclick={() => (runDetail = null)}>{$t("butler.runs.back")}</button>
            <div class="bt-run-detail">
              <h4>{projectName(runDetail.run.project_id)} · {runDetail.run.status}</h4>
              <small>{when(runDetail.run.started_at)} · {duration(runDetail.run)} · {runDetail.run.model}</small>
              {#if runDetail.run.summary}<p>{runDetail.run.summary}</p>{/if}
              {#each runDetail.messages as m, i (i)}
                <div class="bt-msg">
                  <b>{(m as Record<string, unknown>)?.role ?? "?"}</b>
                  <pre>{msgText(m).slice(0, 1200)}</pre>
                </div>
              {/each}
            </div>
          {:else if runs.length === 0}
            <p class="bt-empty">{$t("butler.runs.empty")}</p>
          {:else}
            {#each runs as run (run.run_id)}
              <div class="bt-run" role="button" tabindex="0" onclick={() => void openRun(run)} onkeydown={(e) => e.key === "Enter" && void openRun(run)}>
                <div class="bt-run-main">
                  <b>{projectName(run.project_id)} · {run.status}</b>
                  <small>{when(run.started_at)} · {duration(run)} · {run.model || $t("butler.runs.defaultModel")}</small>
                  {#if run.summary}<small>{run.summary.slice(0, 140)}</small>{/if}
                </div>
              </div>
            {/each}
          {/if}
        </section>
      {:else if tab === "automations"}
        <section class="bt-sec">
          {#if automations.length === 0}
            <p class="bt-empty">{$t("butler.automations.empty")}</p>
          {:else}
            {#each automations as a, i (i)}
              <div class="bt-run">
                <div class="bt-run-main">
                  <b>{a.repeat_count > 1 ? $t("butler.automations.heartbeat") : $t("butler.automations.reminder")}</b>
                  <small>{a.message}</small>
                  <small>{when(a.fire_at_ms)} · {a.repeat_count} 次 · {a.repeat_interval_secs}s</small>
                </div>
                <button class="bt-btn" onclick={() => void cancelReminder(i, a.id)}>{$t("butler.automations.cancel")}</button>
              </div>
            {/each}
          {/if}
          <p class="bt-hint">
            {$t("butler.goals")}: {goals.active} {$t("butler.active")} / {goals.total} {$t("butler.total")} · {$t("butler.stewardTickets")}: {steward.open} {$t("butler.openCount")} / {steward.total} {$t("butler.total")}
          </p>
          {#if platforms.length}
            <p class="bt-hint">
              平台：{platforms.map((p) => `${p.name}${p.enabled ? "（启用）" : "（停用）"}`).join(" · ")}
            </p>
          {/if}
          <p class="bt-hint">{$t("butler.automations.systems")}</p>
        </section>
      {:else if tab === "approvals"}
        <section class="bt-sec">
          {#if approvals.length === 0}
            <p class="bt-empty">{$t("butler.approvals.empty")}</p>
          {:else}
            {#each approvals as a (a.id)}
              <div class="bt-approval">
                <div class="bt-approval-main">
                  <b>{a.action}</b>
                  <small>{a.reversibility}{a.impact ? ` · ${a.impact}` : ""}</small>
                </div>
                <div class="bt-approval-actions">
                  <button class="bt-btn ok" onclick={() => void decide(a.id, true)}>
                    {$t("butler.approve")}
                  </button>
                  <button class="bt-btn" onclick={() => void decide(a.id, false)}>
                    {$t("butler.deny")}
                  </button>
                </div>
              </div>
            {/each}
          {/if}
        </section>
      {/if}
    </div>
  </div>
{/if}

<style>
  .bt-overlay { position: fixed; inset: 0; background: rgba(5, 7, 10, 0.62); display: flex; justify-content: flex-end; z-index: 60; }
  .bt-panel { width: min(820px, 96vw); height: 100%; overflow-y: auto; background: #0f1319; border-left: 1px solid #232a35; padding: 22px 22px 40px; color: #c3cbd8; font-size: 13.5px; }
  .bt-head { display: flex; align-items: flex-start; justify-content: space-between; margin-bottom: 16px; }
  .bt-kicker { font-size: 10.5px; letter-spacing: 0.18em; text-transform: uppercase; color: #cbb26a; margin-bottom: 6px; }
  h2 { margin: 0; font-size: 20px; font-weight: 600; color: #e6ebf2; }
  .bt-icon { background: transparent; border: 0; color: #98a2b2; font-size: 20px; cursor: pointer; line-height: 1; }
  .bt-toolbar { display: flex; align-items: center; gap: 8px; padding: 10px 0; border-top: 1px solid #232a35; border-bottom: 1px solid #232a35; flex-wrap: wrap; }
  .bt-dot { width: 7px; height: 7px; border-radius: 50%; background: #3a4352; }
  .bt-dot.on { background: #5fd3a3; }
  .bt-state { font-size: 12px; color: #98a2b2; }
  .bt-spacer { flex: 1; }
  .bt-btn, .bt-primary { background: #161c25; border: 1px solid #2e3746; color: #c3cbd8; border-radius: 5px; padding: 5px 10px; font-size: 12px; cursor: pointer; }
  .bt-btn:hover { border-color: #4a5668; }
  .bt-btn.ok { color: #5fd3a3; border-color: #2f5b4b; }
  .bt-primary { background: #cbb26a; border-color: #cbb26a; color: #14181f; font-weight: 600; }
  .bt-error { color: #e2836f; background: #1d1414; border: 1px solid #3d2424; padding: 8px 10px; border-radius: 5px; }
  .bt-stats { display: flex; gap: 14px; flex-wrap: wrap; padding: 10px 0; font-size: 12px; color: #98a2b2; }
  .bt-stats b { color: #e6ebf2; font-weight: 600; }
  .bt-tabs { display: flex; gap: 4px; border-bottom: 1px solid #232a35; margin-bottom: 12px; }
  .bt-tabs button { background: transparent; border: 0; border-bottom: 2px solid transparent; color: #8b96a8; padding: 8px 12px; font-size: 12.5px; cursor: pointer; }
  .bt-tabs button.active { color: #e6ebf2; border-bottom-color: #cbb26a; }
  .bt-tabs em { font-style: normal; background: #cbb26a; color: #14181f; border-radius: 8px; padding: 0 5px; margin-left: 4px; font-size: 10px; }
  .bt-sec { margin-top: 8px; }
  .bt-empty, .bt-hint { color: #6f7a8a; font-size: 12px; }
  .bt-project, .bt-task, .bt-run, .bt-approval { display: flex; gap: 10px; align-items: flex-start; padding: 10px 0; border-bottom: 1px solid #1d232d; }
  .bt-project { justify-content: space-between; }
  .bt-project-main, .bt-task-main, .bt-run-main, .bt-approval-main { display: flex; flex-direction: column; gap: 3px; min-width: 0; }
  .bt-project-main small, .bt-task-main small, .bt-run-main small, .bt-approval-main small { color: #8b96a8; font-size: 11.5px; word-break: break-all; }
  .bt-project-actions { display: flex; gap: 6px; flex-shrink: 0; }
  .bt-proj { color: #cbb26a; margin-right: 6px; }
  .bt-pill { font-size: 10.5px; padding: 2px 7px; border-radius: 999px; border: 1px solid #2e3746; color: #98a2b2; flex-shrink: 0; }
  .bt-pill.running, .bt-pill.verifying { color: #e0b263; border-color: #5a4a2a; }
  .bt-pill.verified { color: #5fd3a3; border-color: #2f5b4b; }
  .bt-pill.failed { color: #e2836f; border-color: #5a3232; }
  .bt-pill.blocked { color: #cbb26a; border-color: #5a4f2a; }
  .bt-task { cursor: pointer; }
  .bt-task:hover { background: #141a22; }
  .bt-task-detail { margin-top: 6px; padding: 8px 10px; background: #0d1117; border: 1px solid #232a35; border-radius: 5px; font-size: 11.5px; color: #aab5c5; }
  .bt-task-detail p { margin: 0 0 6px; white-space: pre-wrap; word-break: break-word; }
  .bt-events { display: flex; flex-direction: column; gap: 3px; margin: 6px 0; max-height: 180px; overflow: auto; }
  .bt-events span { color: #6f7a8a; }
  .bt-run { cursor: pointer; }
  .bt-run:hover { background: #141a22; }
  .bt-run-detail h4 { margin: 10px 0 4px; color: #e6ebf2; }
  .bt-msg { border-top: 1px solid #1d232d; padding: 8px 0; }
  .bt-msg pre { white-space: pre-wrap; word-break: break-word; background: #0d1117; border: 1px solid #232a35; padding: 8px; font-size: 11px; color: #b9c4d4; max-height: 220px; overflow: auto; }
  .bt-approval { justify-content: space-between; }
  .bt-approval-actions { display: flex; gap: 6px; }
</style>
