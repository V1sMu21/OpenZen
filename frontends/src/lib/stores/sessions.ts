import { writable, get } from "svelte/store";
import type { SessionInfo } from "../api/sessions";
import { listSessions, createSession, deleteSession, renameSession as renameSessionApi } from "../api/sessions";

function createSessionStore() {
  const { subscribe, set, update } = writable<{
    sessions: SessionInfo[];
    currentId: string | null;
    loading: boolean;
  }>({
    sessions: [],
    currentId: null,
    loading: false,
  });

  return {
    subscribe,

    async load(projectId?: string) {
      update((s) => ({ ...s, loading: true }));
      try {
        const sessions = await listSessions(projectId);
        update((s) => ({ ...s, sessions, loading: false }));
      } catch {
        update((s) => ({ ...s, loading: false }));
      }
    },

    async create(name?: string, projectId?: string) {
      const result = await createSession(name);
      await this.load();
      this.select(result.session_id);
      return result;
    },

    async remove(id: string) {
      await deleteSession(id);
      update((s) => {
        const sessions = s.sessions.filter((ss) => ss.id !== id);
        const currentId = s.currentId === id ? null : s.currentId;
        return { ...s, sessions, currentId };
      });
    },

    select(id: string) {
      update((s) => ({ ...s, currentId: id }));
    },

    previous(): string | null {
      let result: string | null = null;
      update((s) => {
        const idx = s.sessions.findIndex((ss) => ss.id === s.currentId);
        if (idx > 0) {
          result = s.sessions[idx - 1].id;
          return { ...s, currentId: result };
        }
        return s;
      });
      return result;
    },

    next(): string | null {
      let result: string | null = null;
      update((s) => {
        const idx = s.sessions.findIndex((ss) => ss.id === s.currentId);
        if (idx >= 0 && idx < s.sessions.length - 1) {
          result = s.sessions[idx + 1].id;
          return { ...s, currentId: result };
        }
        return s;
      });
      return result;
    },

    /** Increment a session's displayed message count locally. Used after
     *  sendMessage so the sidebar count updates without a full
     *  list_sessions round-trip on every message. */
    bumpMessageCount(id: string, delta = 1) {
      update((s) => ({
        ...s,
        sessions: s.sessions.map((ss) =>
          ss.id === id
            ? { ...ss, message_count: Math.max(0, ss.message_count + delta) }
            : ss,
        ),
      }));
    },

    async rename(id: string, name: string) {
      await renameSessionApi(id, name);
      update((s) => ({
        ...s,
        sessions: s.sessions.map((ss) => ss.id === id ? { ...ss, name } : ss),
      }));
    },
  };
}

export const sessions = createSessionStore();
export type SessionsStore = ReturnType<typeof createSessionStore>;
