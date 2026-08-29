// Settings panel API wrappers (docs/settings-panel-plan.md).
//
// These commands are desktop-only (they read/write local config files and
// process state), so unlike chat.ts there is deliberately no HTTP fallback:
// the panel is only reachable from the Tauri shell and wrappers reject
// otherwise instead of silently returning empty data.

import { isTauri, tauriInvoke } from "./tauri";
import type {
  McpServerItem,
  ModelEntry,
  ModelUpsertArgs,
  SkillMcpItem,
  TokenStats,
} from "./chat";

/** Minimal mirror of the get_memory_status soul section (commands.rs). */
export interface SoulStatus {
  enabled: boolean;
  soul?: {
    identity: string;
    mood: string;
    confidence: number;
    portrait_facts: number;
    narrative_chapters: number;
    version: number;
  };
  store?: {
    total_entries: number;
    l1_entries: number;
    l2_entries: number;
    l3_entries: number;
    recalls: number;
    recall_hits: number;
    recall_hit_rate: number;
  };
}

type MutationResult = { status?: string; error?: string };

const BIRTH_NAME_PREFIX = "记忆体 · 醒于";
const DEFAULT_IDENTITY = "未命名的记忆体";

/** The user-given agent name from soul.identity, or null while the soul
 *  still carries its auto-assigned birth name ("记忆体 · 醒于 …",
 *  vendor entropy-memory-engine core.rs birth_name) or the placeholder. */
export function soulDisplayName(status: SoulStatus | null | undefined): string | null {
  const id = status?.soul?.identity?.trim();
  if (!id || id === DEFAULT_IDENTITY || id.startsWith(BIRTH_NAME_PREFIX)) return null;
  return id;
}

async function invoke<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  if (!isTauri()) throw new Error(`${cmd} is only available in the desktop app`);
  return (await tauriInvoke(cmd, args)) as T;
}

function unwrapError(res: MutationResult, cmd: string): MutationResult {
  if (res.error) throw new Error(`${cmd}: ${res.error}`);
  return res;
}

export function fetchModels(): Promise<ModelEntry[]> {
  return invoke<ModelEntry[]>("list_models");
}

export async function upsertModel(args: ModelUpsertArgs): Promise<MutationResult> {
  return unwrapError(await invoke<MutationResult>("upsert_model", { args }), "upsert_model");
}

export async function deleteModel(name: string): Promise<MutationResult> {
  return unwrapError(await invoke<MutationResult>("delete_model", { name }), "delete_model");
}

export async function setDefaultModel(name: string): Promise<MutationResult> {
  return unwrapError(await invoke<MutationResult>("set_default_model", { name }), "set_default_model");
}

export function listSkillMcp(): Promise<{ busy?: boolean; skills: SkillMcpItem[]; sops: SkillMcpItem[] }> {
  return invoke("list_skill_mcp");
}

export async function toggleSkillMcp(
  kind: "skill" | "sop",
  name: string,
  active: boolean,
): Promise<MutationResult> {
  return unwrapError(await invoke<MutationResult>("toggle_skill_mcp", { kind, name, active }), "toggle_skill_mcp");
}

export function listMcpServers(): Promise<{ servers: McpServerItem[] }> {
  return invoke("list_mcp_servers");
}

export async function toggleMcpServer(name: string, enabled: boolean): Promise<MutationResult> {
  return unwrapError(await invoke<MutationResult>("toggle_mcp_server", { name, enabled }), "toggle_mcp_server");
}

export function getTokenStats(limit: number = 50): Promise<TokenStats> {
  return invoke<TokenStats>("get_token_stats", { limit });
}

export function fetchSoulStatus(): Promise<SoulStatus> {
  return invoke<SoulStatus>("get_memory_status");
}

export async function setSoulIdentity(name: string): Promise<MutationResult> {
  return unwrapError(await invoke<MutationResult>("set_soul_identity", { name }), "set_soul_identity");
}
