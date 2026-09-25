// Per-turn aggregation of the files an assistant turn actually modified.
//
// The backend emits no file-change event and the file tools report only
// `{"status":"edited"}`, so the change set is reconstructed on the client from
// the tool-call parts themselves — the same source EditCard renders from.
// Aggregating here (rather than summing the rendered EditCards) also survives
// the activity-timeline fold in ChatMessage.svelte, which hides older cards
// once a turn exceeds FOLD_THRESHOLD.

import type { UIMessagePart } from "../stores/parts";
import { computeDiff, diffStat } from "./diff";
import {
  basename,
  displayPath,
  normalizePath,
  resolveAgainstWorkingDir,
} from "./paths";

/** Tools that mutate a file on disk.
 *
 *  `write`/`edit`/`patch` are the registered names
 *  (crates/oz-tools/src/lib.rs `build_default`); the `file_*` spellings are the
 *  aliases the backend's quality gate still accepts
 *  (crates/oz-core/src/quality.rs:575), so archived sessions that used them
 *  are covered too. */
const FILE_EDIT_TOOLS = new Set([
  "edit", "patch", "write",
  "file_edit", "file_patch", "file_write",
]);

/** Is this tool call a file modification that should render an edit card and
 *  count towards the turn's changed files? Both call sites ask here so the
 *  aggregate card can never disagree with the inline cards. */
export function isFileEditTool(name: string): boolean {
  return FILE_EDIT_TOOLS.has(name);
}

export interface TurnFile {
  /** Resolved path — the identity key, and what the side panel is asked to
   *  open. Relative tool paths are resolved against the working dir here
   *  (see `resolveAgainstWorkingDir`). */
  path: string;
  /** Human label: relative to the working dir when the file lives inside it. */
  display: string;
  added: number;
  removed: number;
  /** Successful tool calls that touched this file. */
  edits: number;
  /** Whether `added`/`removed` were actually measured. False only for a
   *  deliverable the agent rewrote through a script (`code_run` shell/python
   *  heredoc): the file is known to have been produced, but its line counts
   *  never cross the tool-args boundary, and a fabricated +0 −0 would read as
   *  "nothing changed". Such a row shows a label instead of a diffstat. */
  hasStats: boolean;
  /** Deliverable rows only: the agent's own label for the artifact. */
  label?: string;
  /** Deliverable rows only: the artifact_type the agent declared (validated
   *  backend-side against the renderer list). Preferred over the extension
   *  map when opening, since e.g. a `.json` data file is served better by the
   *  declared `spreadsheet` viewer than by `code`. */
  type?: string;
}

const EMPTY: TurnFile[] = [];

/** `parts` is treated as immutable persisted data, so the computed list is
 *  cached on the array itself. Without this, every unrelated store update that
 *  hands ChatMessage a fresh array would re-run an LCS per edited file. */
const cache = new WeakMap<UIMessagePart[], { workingDir: string; files: TurnFile[] }>();

interface EditArgs {
  filePath: string;
  oldString: string;
  newString: string;
}

/** Tool args → the two sides of the edit, or null when the call is not a
 *  usable file write (unparseable / still-streaming / no path).
 *
 *  Shared with ChatMessage's EditCard so a file's card and its entry in the
 *  changed-files card are always derived from the same fields. */
export function parseEditArgs(raw: string | undefined, toolName: string): EditArgs | null {
  if (!raw) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!parsed || typeof parsed !== "object") return null;
  const o = parsed as Record<string, unknown>;
  const filePath = typeof o.file_path === "string" ? o.file_path : "";
  if (!filePath.trim()) return null;
  if (toolName === "write" || toolName === "file_write") {
    // A whole-file write has no `old_string`: the side we can see is the new
    // content, so a create/overwrite counts as additions only.
    return {
      filePath,
      oldString: "",
      newString: typeof o.content === "string" ? o.content : "",
    };
  }
  return {
    filePath,
    oldString: typeof o.old_string === "string" ? o.old_string : "",
    newString: typeof o.new_string === "string" ? o.new_string : "",
  };
}

/** Did the call fail — i.e. change nothing on disk?
 *
 *  Shared with EditCard's red/green verdict so a file can never be counted as
 *  changed while its own card says 失败.
 *
 *  Result payloads are normally JSON envelopes: `{"status":"written"|"edited"|
 *  "patched"}` on success, `{"error":…}` or `{"status":"error",…}` on failure
 *  (`ToolOutput::bad_json`, crates/oz-core-types/src/tool.rs:186). `null` means
 *  the result event never arrived (interrupted save) — no evidence of failure,
 *  so the edit is counted rather than silently dropped.
 *
 *  The unparseable branch is load-bearing: one backend path used to build its
 *  error envelope by hand — `format!("{{\"error\":\"{}\"}}", e)`
 *  (agent_loop.rs, since fixed to serialise through serde) — which emitted
 *  INVALID JSON whenever the error message contained a quote or a newline.
 *  Sessions persisted before that fix still contain such envelopes, and a
 *  future hand-rolled producer could reintroduce them, so they must still read
 *  as failures. The substring test cannot misfire on success: for the file
 *  tools a successful result is always a JSON status object, never free text. */
export function isFailedToolResult(result: unknown): boolean {
  if (result == null) return false;

  const isErrorEnvelope = (o: Record<string, unknown>): boolean =>
    o.status === "error" || (typeof o.error === "string" && o.error.length > 0);

  if (typeof result === "object") {
    if (isErrorEnvelope(result as Record<string, unknown>)) return true;
  }

  const text = typeof result === "string" ? result : JSON.stringify(result);
  try {
    const parsed: unknown = JSON.parse(text);
    if (parsed && typeof parsed === "object") {
      // Well-formed payload: its verdict is final, no substring guessing.
      return isErrorEnvelope(parsed as Record<string, unknown>);
    }
  } catch {
    /* not JSON — fall through to the text heuristic */
  }
  const lower = text.toLowerCase();
  return lower.includes("error") || lower.includes("failed");
}

/** Files modified by the tool calls in `parts`, most-changed first. */
export function collectChangedFiles(
  parts: UIMessagePart[] | undefined,
  workingDir: string,
): TurnFile[] {
  if (!parts || parts.length === 0) return EMPTY;

  const cached = cache.get(parts);
  if (cached && cached.workingDir === workingDir) return cached.files;

  const byPath = new Map<string, TurnFile>();

  for (const part of parts) {
    if (part.type !== "tool-invocation") continue;
    if (!isFileEditTool(part.name)) continue;
    if (isFailedToolResult(part.result)) continue;

    const args = parseEditArgs(part.args ?? "", part.name);
    if (!args) continue;

    const oldLines = args.oldString.length > 0 ? args.oldString.split("\n") : [];
    const newLines = args.newString.length > 0 ? args.newString.split("\n") : [];
    const stat = diffStat(computeDiff(oldLines, newLines));

    const path = resolveAgainstWorkingDir(args.filePath, workingDir);
    const existing = byPath.get(path);
    if (existing) {
      existing.added += stat.added;
      existing.removed += stat.removed;
      existing.edits += 1;
    } else {
      byPath.set(path, {
        path,
        display: displayPath(normalizePath(args.filePath), workingDir),
        added: stat.added,
        removed: stat.removed,
        edits: 1,
        hasStats: true,
      });
    }
  }

  const files = [...byPath.values()].sort(
    (a, b) =>
      b.added + b.removed - (a.added + a.removed) || a.display.localeCompare(b.display),
  );
  cache.set(parts, { workingDir, files });
  return files;
}

// ── Deliverables ──
//
// The agent's "here is what I made for you" signal is `open_side_panel`
// (crates/oz-tools/src/open_side_panel.rs), which opens a file in the right
// sidebar and reports `{status:"OPENED", artifact_path, artifact_label,
// artifact_type}`. It is the ONLY evidence a turn produced something when the
// agent built it with a script: a `code_run` python heredoc rewrites the file
// on disk without leaving tool args to measure, so an arg-only collector shows
// a card that omits the very file the turn existed to deliver.

const DELIVERABLE_TOOL = "open_side_panel";

export interface Deliverable {
  path: string;
  display: string;
  /** The agent's label, falling back to the file name. */
  label: string;
  /** Declared artifact_type, or "" when the call omitted it. */
  type: string;
}

interface DeliverableArgs {
  path: string;
  label: string;
  type: string;
}

/** `open_side_panel` args → the artifact it announced, or null when the call
 *  is unusable: unparseable, still streaming, path-less, or a `terminal`
 *  artifact — whose path is `.` (the working dir itself) and therefore names
 *  no file the card could open. */
export function parseDeliverableArgs(raw: string | undefined): DeliverableArgs | null {
  if (!raw) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!parsed || typeof parsed !== "object") return null;
  const o = parsed as Record<string, unknown>;
  const path = typeof o.artifact_path === "string" ? o.artifact_path : "";
  if (!path.trim()) return null;
  const type = typeof o.artifact_type === "string" ? o.artifact_type : "";
  if (type === "terminal") return null;
  return {
    path,
    label: typeof o.artifact_label === "string" ? o.artifact_label.trim() : "",
    type,
  };
}

const NO_DELIVERABLES: Deliverable[] = [];
const deliverableCache = new WeakMap<
  UIMessagePart[],
  { workingDir: string; items: Deliverable[] }
>();

/** Deliverables announced during this turn, in announcement order.
 *
 *  Deduped by resolved path (a Map keeps the first insertion position while
 *  the last label wins) because the agent re-opens a file it has just
 *  updated — the newest label describes the newest revision. */
export function collectDeliverables(
  parts: UIMessagePart[] | undefined,
  workingDir: string,
): Deliverable[] {
  if (!parts || parts.length === 0) return NO_DELIVERABLES;

  const cached = deliverableCache.get(parts);
  if (cached && cached.workingDir === workingDir) return cached.items;

  const byPath = new Map<string, Deliverable>();

  for (const part of parts) {
    if (part.type !== "tool-invocation") continue;
    if (part.name !== DELIVERABLE_TOOL) continue;
    if (isFailedToolResult(part.result)) continue;

    const args = parseDeliverableArgs(part.args ?? "");
    if (!args) continue;

    const path = resolveAgainstWorkingDir(args.path, workingDir);
    const normalized = normalizePath(args.path);
    const previous = byPath.get(path);
    byPath.set(path, {
      path,
      display: displayPath(normalized, workingDir),
      // Either side of the fallback keeps the earlier announcement: a repeat
      // call that omits the label (or the type) must not degrade the row to a
      // bare file name.
      label: args.label || previous?.label || basename(normalized),
      type: args.type || previous?.type || "",
    });
  }

  const items = [...byPath.values()];
  deliverableCache.set(parts, { workingDir, items });
  return items;
}

const NO_TURN_FILES: TurnFile[] = [];
const turnCache = new WeakMap<
  UIMessagePart[],
  { workingDir: string; rows: TurnFile[] }
>();

/** Every row this turn's card shows: the deliverables the agent announced
 *  first (the turn's outcome), then the files it edited.
 *
 *  A path in both sets becomes ONE row carrying the agent's label and the line
 *  counts that could be measured, so a deliverable the agent both wrote and
 *  showcased is never listed twice. */
export function collectTurnFiles(
  parts: UIMessagePart[] | undefined,
  workingDir: string,
): TurnFile[] {
  if (!parts || parts.length === 0) return NO_TURN_FILES;

  const cached = turnCache.get(parts);
  if (cached && cached.workingDir === workingDir) return cached.rows;

  const changed = collectChangedFiles(parts, workingDir);
  const deliverables = collectDeliverables(parts, workingDir);

  let rows: TurnFile[];
  if (deliverables.length === 0) {
    rows = changed;
  } else {
    const measured = new Map(changed.map((f) => [f.path, f]));
    const merged: TurnFile[] = [];
    for (const d of deliverables) {
      const stat = measured.get(d.path);
      if (stat) measured.delete(d.path);
      merged.push({
        path: d.path,
        display: d.display,
        added: stat?.added ?? 0,
        removed: stat?.removed ?? 0,
        edits: stat?.edits ?? 0,
        hasStats: stat !== undefined,
        label: d.label,
        type: d.type || undefined,
      });
    }
    // `changed` is already churn-sorted; the delete above keeps that order for
    // whatever the deliverables did not already cover.
    for (const f of changed) {
      if (measured.has(f.path)) merged.push(f);
    }
    rows = merged;
  }

  turnCache.set(parts, { workingDir, rows });
  return rows;
}
