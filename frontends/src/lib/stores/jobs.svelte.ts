// Background jobs (UI mirror) — one store for the composer strip.
//
// The backend emits `job://update` on every start/finish; `job_list` hydrates
// when a session is opened. Ownership is enforced server-side — this mirror
// never decides who may see a job.

import { listen } from "@tauri-apps/api/event";
import { isTauri, tauriInvoke } from "../api/tauri";
import { sessions } from "./sessions";

export interface JobView {
  id: string;
  kind: string;
  label: string;
  status: "running" | "stopping" | "completed" | "killed" | "failed";
  detail?: string | null;
  owner_session: string;
  started_at: number;
  finished_at?: number | null;
}

function createJobs() {
  let items = $state<JobView[]>([]);
  let currentSession = $state<string | null>(null);
  let started = false;

  function upsert(job: JobView) {
    // Never mix owners, even if an event raced the session switch. With no
    // session selected there is nothing to show, so drop the event too.
    if (!currentSession || job.owner_session !== currentSession) return;
    const i = items.findIndex((j) => j.id === job.id);
    if (i >= 0) items[i] = job;
    else items.push(job);
    // Keep only the last 20 so a long session cannot grow this unbounded.
    if (items.length > 20) items = items.slice(items.length - 20);
  }

  async function init() {
    if (started) return;
    started = true;
    if (!isTauri()) return;
    try {
      await listen<{ event: "started" | "finished"; job: JobView }>("job://update", (e) => {
        // Another conversation's job must not leak into this strip: its ✕
        // would call job_kill with the wrong owner and fail silently.
        upsert(e.payload.job);
      });
    } catch {
      // webui / dev: no event bus
    }
  }

  async function load(sessionId: string | null) {
    currentSession = sessionId;
    if (!isTauri() || !sessionId) {
      items = [];
      return;
    }
    // Clear immediately: during the await the previous session's chips
    // would otherwise stay on screen under the new conversation.
    items = [];
    try {
      const res = (await tauriInvoke("job_list", { sessionId })) as { jobs?: JobView[] };
      // Belt and braces: only this owner's jobs, even if a backend query
      // ever widens its scope.
      items = (res?.jobs ?? []).filter((j) => j.owner_session === sessionId);
    } catch {
      items = [];
    }
  }

  async function kill(id: string) {
    if (!isTauri() || !currentSession) return;
    try {
      await tauriInvoke("job_kill", { sessionId: currentSession, jobId: id });
    } catch {
      // Re-sync instead of leaving a ✕ that silently does nothing.
      await load(currentSession);
    }
  }

  return {
    get items() {
      // Render-side fence: a stale event or a race must never show another
      // conversation's jobs in this one.
      return currentSession ? items.filter((j) => j.owner_session === currentSession) : [];
    },
    /** Background subagents are jobs too, but they are represented by the
     *  subagent strip — counting them here made 2 jobs + 2 subagents read
     *  as "4 个后台任务". */
    get backgroundItems() {
      return this.items.filter((j) => j.kind !== "subagent");
    },
    get active() {
      return this.backgroundItems.filter(
        (j) => j.status === "running" || j.status === "stopping",
      );
    },
    init,
    load,
    kill,
  };
}

export const jobs = createJobs();

// Hydrate/clear whenever the current conversation changes.
let lastSession: string | null | undefined;
sessions.subscribe((s) => {
  const id = s.currentId ?? null;
  if (id !== lastSession) {
    lastSession = id;
    void jobs.load(id);
  }
});
