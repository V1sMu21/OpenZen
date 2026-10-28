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
    // Never mix owners, even if an event raced the session switch.
    if (currentSession && job.owner_session !== currentSession) return;
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
        if (currentSession && e.payload.job.owner_session !== currentSession) return;
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
    try {
      const res = (await tauriInvoke("job_list", { sessionId })) as { jobs?: JobView[] };
      items = res?.jobs ?? [];
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
      return items;
    },
    get active() {
      return items.filter((j) => j.status === "running" || j.status === "stopping");
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
