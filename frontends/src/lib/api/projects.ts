export interface ProjectRecord {
  id: string;
  name: string;
  root_path: string;
  created_at: string;
  session_count: number;
  broken?: boolean;
}

export interface ProjectWithSessions extends ProjectRecord {
  sessions: import("./sessions").SessionInfo[];
}

import { isTauri, tauriInvoke } from "./tauri";
import { fetchJson } from "./chat";

const BASE = "/api/projects";

export async function listProjects(): Promise<ProjectRecord[]> {
  if (isTauri()) {
    return (await tauriInvoke("list_projects", {})) ?? [];
  }
  const res = await fetchJson(BASE);
  if (!res.ok) throw new Error(`Failed to list projects: ${res.status}`);
  const data = await res.json();
  return data.projects ?? data ?? [];
}

export async function addProject(
  rootPath: string,
  name?: string,
): Promise<ProjectRecord> {
  if (isTauri()) {
    return await tauriInvoke("add_project", { rootPath, name: name || null });
  }
  const res = await fetchJson(BASE, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ root_path: rootPath, name }),
  });
  if (!res.ok) throw new Error(`Failed to add project: ${res.status}`);
  return res.json();
}

export async function removeProject(projectId: string): Promise<void> {
  if (isTauri()) {
    await tauriInvoke("remove_project", { projectId });
    return;
  }
  const res = await fetchJson(`${BASE}/${projectId}`, { method: "DELETE" });
  if (!res.ok) throw new Error(`Failed to remove project: ${res.status}`);
}

export async function renameProject(
  projectId: string,
  newName: string,
): Promise<void> {
  if (isTauri()) {
    await tauriInvoke("rename_project", { projectId, newName });
    return;
  }
  const res = await fetchJson(`${BASE}/${projectId}`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name: newName }),
  });
  if (!res.ok) throw new Error(`Failed to rename project: ${res.status}`);
}
