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
  ProviderEntry,
  ProviderUpsertArgs,
  SkillMcpItem,
  TokenStats,
} from "./chat";

/** Minimal mirror of the get_memory_status soul section (commands.rs). */
export interface SoulStatus {
  enabled: boolean;
  /** Which embedder the L2 layer actually uses. "hash" is the silent
   *  degradation when the local MLX model is unavailable — semantic
   *  recall quality drops to near-random, so the UI surfaces it. */
  embedding_kind?: string;
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
    l3_storage_bytes?: number;
    hnsw_entries?: number;
    /** Unix nanos of the last consolidation, when one has run. */
    last_consolidation?: number | null;
  };
  harness?: { entry_count: number };

  // ── P1-12 memory-control fields ──────────────────────────────────────
  // The backend is being extended with an explicit memory-control surface
  // (on/off, export, clear, browse). These are OPTIONAL here and read
  // through `memoryStatus()` below, which normalizes both the old and the
  // new shape — so this file works before and after the Rust side lands.
  backend?: string;
  erne_available?: boolean;
  mood?: string | null;
  energy?: number | null;
  confidence?: number | null;
  soul_version?: number;
  user_fact_count?: number;
  self_fact_count?: number;
  self_portrait_updated_at?: string | null;
  last_event_drain_at?: string | null;
  pending_events?: number;
}

/** Normalized, always-present view of the memory status.
 *
 *  Every field is nullable on purpose: the settings panel must be able to
 *  say "未启用 / unknown" instead of rendering a fabricated mood. Values that
 *  the backend does not report stay `null` and the UI hides them. */
export interface MemoryStatusView {
  enabled: boolean;
  backend: string | null;
  erneAvailable: boolean | null;
  mood: string | null;
  energy: number | null;
  confidence: number | null;
  soulVersion: number | null;
  userFactCount: number | null;
  selfFactCount: number | null;
  portraitUpdatedAt: string | null;
  lastDrainAt: string | null;
  pendingEvents: number | null;
  totalEntries: number | null;
  recalls: number | null;
  hitRate: number | null;
}

/** Fold either status shape into one. Never invents a value: a field the
 *  backend did not send stays null so the UI can say "not reported". */
export function memoryStatus(status: SoulStatus | null | undefined): MemoryStatusView {
  const s = status ?? undefined;
  const num = (v: unknown): number | null => (typeof v === "number" ? v : null);
  const str = (v: unknown): string | null =>
    typeof v === "string" && v.trim() !== "" ? v : null;
  return {
    enabled: s?.enabled === true,
    backend: str(s?.backend) ?? str(s?.embedding_kind),
    erneAvailable:
      typeof s?.erne_available === "boolean" ? s.erne_available : null,
    mood: str(s?.mood) ?? str(s?.soul?.mood),
    energy: num(s?.energy),
    confidence: num(s?.confidence) ?? num(s?.soul?.confidence),
    soulVersion: num(s?.soul_version) ?? num(s?.soul?.version),
    userFactCount: num(s?.user_fact_count) ?? num(s?.soul?.portrait_facts),
    selfFactCount: num(s?.self_fact_count),
    portraitUpdatedAt: str(s?.self_portrait_updated_at),
    lastDrainAt: str(s?.last_event_drain_at),
    pendingEvents: num(s?.pending_events),
    totalEntries: num(s?.store?.total_entries),
    recalls: num(s?.store?.recalls),
    hitRate: num(s?.store?.recall_hit_rate),
  };
}

/** One entry in the memory browser. */
export interface MemoryEntry {
  id: string;
  content: string;
  layer?: string;
  importance?: number;
  tags?: string[];
  created_at?: string;
  superseded?: boolean;
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

const BIRTH_NAME_RE = /^记忆体 · 醒于 (\d{4}-\d{2}-\d{2})$/;

/** Localized rendering of the auto-assigned birth name. The engine emits
 *  it in Chinese ("记忆体 · 醒于 YYYY-MM-DD"); re-render per locale so the
 *  English UI never shows Chinese data strings. Returns null for any other
 *  identity (user-given names render as-is). `label` is $t("soul.birthName"). */
export function birthNameDisplay(identity: string, label: string): string | null {
  const m = BIRTH_NAME_RE.exec(identity.trim());
  return m ? label.replace("{date}", m[1]) : null;
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

export function fetchProviders(): Promise<ProviderEntry[]> {
  return invoke<ProviderEntry[]>("list_providers");
}

export async function upsertProvider(args: ProviderUpsertArgs): Promise<MutationResult> {
  return unwrapError(await invoke<MutationResult>("upsert_provider", { args }), "upsert_provider");
}

/** Cascades: model entries referencing the provider are deleted too. */
export async function deleteProvider(
  id: string,
): Promise<MutationResult & { deleted_models?: number }> {
  return unwrapError(await invoke("delete_provider", { id }), "delete_provider");
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

export interface PortraitFact {
  statement: string;
  confidence: number;
}

/** P2-18: the user's view of the agent's portrait facts (correction loop). */
export async function fetchSoulPortrait(): Promise<{
  enabled: boolean;
  facts: PortraitFact[];
}> {
  return invoke("get_soul_portrait");
}

export async function removePortraitFact(statement: string): Promise<MutationResult> {
  return unwrapError(
    await invoke<MutationResult>("remove_portrait_fact", { statement }),
    "remove_portrait_fact",
  );
}

/** Turn long-term memory on/off. Writes mykey.toml and takes effect
 *  immediately; the caller must roll its switch back if this throws. */
export async function setMemoryEnabled(enabled: boolean): Promise<{ ok?: boolean; backend?: string }> {
  return invoke<{ ok?: boolean; backend?: string }>("set_memory_enabled", { enabled });
}

/** Export every memory entry to a JSON file under the data root. */
export async function exportMemory(): Promise<{ path?: string; bytes?: number; entries?: number }> {
  return invoke<{ path?: string; bytes?: number; entries?: number }>("export_memory");
}

/** Delete all memories. Always backs up first (the returned `backup_path`
 *  is what the user can restore from), and the caller must have asked for
 *  confirmation — the backend takes an explicit `confirm` flag for that. */
export async function clearMemory(): Promise<{ ok?: boolean; backup_path?: string; removed?: number }> {
  return invoke<{ ok?: boolean; backup_path?: string; removed?: number }>("clear_memory", {
    confirm: true,
  });
}

/** Browse memories (newest first). `query` filters by substring. */
export async function listMemories(
  limit = 100,
  query = "",
): Promise<{ entries: MemoryEntry[] }> {
  return invoke<{ entries: MemoryEntry[] }>("list_memories", {
    limit,
    query: query.trim() === "" ? null : query.trim(),
  });
}

/** Forget one memory by id. */
export async function forgetMemory(id: string): Promise<{ ok?: boolean; id?: string }> {
  return invoke<{ ok?: boolean; id?: string }>("forget_memory", { id });
}

/** Reveal a path in the OS file manager (Finder).
 *
 *  Three routes, in order of preference:
 *   1. `reveal_path` — the app's own command (requested from the Rust side;
 *      the opener plugin is not a dependency of this frontend);
 *   2. the opener/shell plugin, if it is ever added;
 *   3. clipboard — always available, and "paste into Finder's Go to Folder"
 *      is what the user would do anyway. Returns how it was handled so the
 *      caller can tell the user the truth instead of pretending Finder
 *      opened. */
export async function revealInFileManager(
  path: string,
): Promise<"revealed" | "copied"> {
  const w = window as any;
  if (isTauri()) {
    try {
      await tauriInvoke("reveal_path", { path });
      return "revealed";
    } catch {
      /* command not registered yet — fall through */
    }
  }
  const plugin = w.__TAURI__?.opener ?? w.__TAURI__?.shell;
  if (plugin?.revealItemInDir) {
    await plugin.revealItemInDir(path);
    return "revealed";
  }
  if (navigator?.clipboard?.writeText) {
    await navigator.clipboard.writeText(path);
    return "copied";
  }
  throw new Error("reveal unavailable");
}

/** Delivery-quality counters from the reflexion ledger (R5-5).
 *  `week`/`total` are reflection-type → count maps; `rates_*` are the
 *  denominators-and-rates view. Everything is optional: a fresh install has
 *  an empty ledger and the UI must say "no data yet". */
export interface QualityReport {
  generated_at?: string;
  projects_scanned?: number;
  window_days?: number;
  week?: Record<string, number>;
  total?: Record<string, number>;
  rates_week?: QualityRates;
  rates_total?: QualityRates;
}

export interface QualityRates {
  review_attempts?: number;
  review_pass_rate?: number | null;
  review_rework_rate?: number | null;
  deliveries?: number;
  assertion_failure_rate?: number | null;
  review_coverage?: number | null;
}

/** Read the quality report (also persisted to openzen/quality_report.json). */
export async function fetchQualityReport(): Promise<QualityReport> {
  return invoke<QualityReport>("get_quality_report");
}

/** Forward a frontend error to the backend log (P1-10).
 *
 *  A renderer-side exception used to be invisible: it died in the webview
 *  console, which the user never opens and which is not in openzen.log.
 *  Never throws — a logging failure must not become a second error. */
export async function logFrontend(
  level: "error" | "warn" | "info",
  message: string,
  stack = "",
): Promise<void> {
  if (!isTauri()) return;
  try {
    await tauriInvoke("log_frontend", { level, message, stack });
  } catch {
    /* logging must never throw */
  }
}

/** Bundle logs + config + state into one archive for a bug report. */
export async function exportDiagnostics(): Promise<{ path?: string; bytes?: number }> {
  return invoke<{ path?: string; bytes?: number }>("export_diagnostics");
}

export async function setSoulIdentity(name: string): Promise<MutationResult> {
  return unwrapError(await invoke<MutationResult>("set_soul_identity", { name }), "set_soul_identity");
}
