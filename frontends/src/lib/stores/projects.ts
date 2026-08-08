import { writable, get } from "svelte/store";
import {
  listProjects,
  addProject as addProjectApi,
  removeProject as removeProjectApi,
  renameProject as renameProjectApi,
  type ProjectRecord,
} from "../api/projects";
import type { ProjectWithSessions } from "../api/projects";
export type { ProjectWithSessions, ProjectRecord };
import { listSessions, createSessionInProject, moveSession as moveSessionApi } from "../api/sessions";
import type { SessionInfo } from "../api/sessions";

export interface ProjectStoreState {
  projects: ProjectWithSessions[];
  expandedProjectIds: Set<string>;
  loading: boolean;
  filterText: string;
}

function createProjectStore() {
  const store = writable<ProjectStoreState>({
    projects: [],
    expandedProjectIds: new Set(),
    loading: false,
    filterText: "",
  });
  const { subscribe, set, update } = store;

  async function loadSessionsForProject(projectId: string): Promise<SessionInfo[]> {
    return await listSessions(projectId);
  }

  return {
    subscribe,

    async loadAll() {
      update((s) => ({ ...s, loading: true }));
      try {
        const raw = await listProjects();
        const projects: ProjectWithSessions[] = await Promise.all(
          raw.map(async (p) => ({
            ...p,
            sessions: await loadSessionsForProject(p.id),
          })),
        );
        update((s) => ({ ...s, projects, loading: false }));
      } catch {
        update((s) => ({ ...s, loading: false }));
      }
    },

    async add(rootPath: string, name?: string) {
      const record = await addProjectApi(rootPath, name);
      const sessions = await loadSessionsForProject(record.id);
      update((s) => ({
        ...s,
        projects: [...s.projects, { ...record, sessions }],
      }));
    },

    async remove(id: string) {
      await removeProjectApi(id);
      update((s) => ({
        ...s,
        projects: s.projects.filter((p) => p.id !== id),
      }));
    },

    async rename(id: string, newName: string) {
      await renameProjectApi(id, newName);
      update((s) => ({
        ...s,
        projects: s.projects.map((p) =>
          p.id === id ? { ...p, name: newName } : p,
        ),
      }));
    },

    toggleExpand(id: string) {
      update((s) => {
        const next = new Set(s.expandedProjectIds);
        if (next.has(id)) {
          next.delete(id);
        } else {
          next.add(id);
        }
        return { ...s, expandedProjectIds: next };
      });
    },

    setFilter(text: string) {
      update((s) => ({ ...s, filterText: text }));
    },

    async createSessionIn(projectId: string, name?: string) {
      const result = await createSessionInProject(projectId, name);
      const sessions = await loadSessionsForProject(projectId);
      update((s) => ({
        ...s,
        projects: s.projects.map((p) =>
          p.id === projectId ? { ...p, sessions } : p,
        ),
      }));
      return result;
    },

    async moveSession(sessionId: string, toProjectId: string) {
      await moveSessionApi(sessionId, toProjectId);
      const currentState = get(store);
      const fromProject = currentState.projects.find(
        (p) => p.sessions.some((s) => s.id === sessionId)
      );
      const fromId = fromProject?.id;
      if (fromId) {
        const fromSessions = fromId === toProjectId
          ? currentState.projects.find(p => p.id === fromId)?.sessions ?? []
          : await loadSessionsForProject(fromId);
        const toSessions = await loadSessionsForProject(toProjectId);
        update((s) => ({
          ...s,
          projects: s.projects.map((p) => {
            if (p.id === toProjectId) return { ...p, sessions: toSessions };
            if (p.id === fromId) return { ...p, sessions: fromSessions };
            return p;
          }),
        }));
      }
    },

    removeSessionFromProject(sessionId: string, projectId: string) {
      update((s) => ({
        ...s,
        projects: s.projects.map((p) =>
          p.id === projectId
            ? { ...p, sessions: p.sessions.filter((s) => s.id !== sessionId) }
            : p,
        ),
      }));
    },
  };
}

export const projects = createProjectStore();
