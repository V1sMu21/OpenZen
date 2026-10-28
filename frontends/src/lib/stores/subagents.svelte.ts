// Subagent mirror for the chat strip + background panel.
//
// Backend emits `subagent://update` on start/finish; `agent_list` hydrates the
// sidecar meta. Transcriptions are fetched on demand (`agent_read`).

import { listen } from "@tauri-apps/api/event";
import { isTauri, tauriInvoke } from "../api/tauri";
import { sessions } from "./sessions";

export interface SubagentView {
  id: string;
  parent: string;
  label: string;
  status: "running" | "completed" | "failed" | "interrupted";
  needs_input?: boolean;
  finished_at?: number;
}

function createSubagents() {
  let items = $state<SubagentView[]>([]);
  let currentSession = $state<string | null>(null);
  let started = false;

  function upsert(s: SubagentView) {
    if (!currentSession || s.parent !== currentSession) return;
    const i = items.findIndex((x) => x.id === s.id);
    if (i >= 0) items[i] = { ...items[i], ...s };
    else items.push(s);
  }

  async function init() {
    if (started) return;
    started = true;
    if (!isTauri()) return;
    try {
      await listen<{ event: string; child: SubagentView }>("subagent://update", (e) => {
        upsert(e.payload.child);
      });
    } catch {
      /* dev */
    }
  }

  async function load(sessionId: string | null) {
    currentSession = sessionId;
    if (!isTauri() || !sessionId) {
      items = [];
      return;
    }
    items = [];
    try {
      const res = (await tauriInvoke("agent_list", { parentSession: sessionId })) as {
        agents?: SubagentView[];
      };
      items = res?.agents ?? [];
    } catch {
      items = [];
    }
  }

  async function transcript(id: string): Promise<string> {
    if (!isTauri()) return "";
    try {
      return (await tauriInvoke("agent_read", { childId: id })) as string;
    } catch (e) {
      return String(e);
    }
  }

  return {
    get items() {
      return currentSession ? items.filter((s) => s.parent === currentSession) : [];
    },
    get active() {
      return items.filter((s) => s.status === "running");
    },
    get needsInput() {
      return items.filter((s) => s.needs_input);
    },
    init,
    load,
    transcript,
  };
}

export const subagents = createSubagents();

let last: string | null | undefined;
sessions.subscribe((s) => {
  const id = s.currentId ?? null;
  if (id !== last) {
    last = id;
    void subagents.load(id);
  }
});
