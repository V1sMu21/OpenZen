// Shared path helpers for tool-call / artifact rendering.
//
// These used to be private copies inside ToolCallCard.svelte and
// EditCard.svelte; the changed-files card needs the same three functions, so
// they live here instead of a third copy.

/** Last `/`-separated segment of a path (the path itself when it has none). */
export function basename(path: string): string {
  const parts = path.split("/");
  return parts[parts.length - 1] || path;
}

/** Everything before the last `/`, or "" when the path has none. */
export function dirname(path: string): string {
  const idx = path.lastIndexOf("/");
  return idx >= 0 ? path.slice(0, idx) : "";
}

/** Display form of a path: equal to the working dir → only its directory
 *  name; inside the working dir → the relative remainder; otherwise verbatim. */
export function displayPath(p: string, workingDir: string): string {
  if (!workingDir) return p;
  const base = workingDir.replace(/\/+$/, "");
  if (!base) return p;
  if (p === base) return p.split("/").pop() || p;
  if (p.startsWith(base + "/")) return p.slice(base.length + 1);
  return p;
}

/** Lexical path cleanup: collapse repeated `/`, drop `.` segments and resolve
 *  `..` against the preceding segment. No filesystem access, so a path that
 *  does not exist yet still normalises.
 *
 *  Used to KEY the changed-file set, so the same file reached as `./src/a.ts`,
 *  `src/a.ts` and `src//a.ts` collapses into one row instead of three rows
 *  with split `+N/-M`.
 *
 *  `displayPath` deliberately does NOT normalise, so displayed paths stay
 *  exactly as the tool call spelled them. */
export function normalizePath(p: string): string {
  if (!p) return p;
  const absolute = p.startsWith("/");
  const out: string[] = [];
  for (const seg of p.split("/")) {
    if (seg === "" || seg === ".") continue;
    if (seg === "..") {
      // Above an absolute root is still the root; a relative path keeps the
      // `..` so it cannot silently become something else.
      if (out.length > 0 && out[out.length - 1] !== "..") out.pop();
      else if (!absolute) out.push("..");
      continue;
    }
    out.push(seg);
  }
  if (absolute) return "/" + out.join("/");
  return out.length > 0 ? out.join("/") : ".";
}

/** Absolute, normalised form of `p`.
 *
 *  The agent's file tools resolve a relative path against the agent working
 *  dir (`is_in_working_dir`, crates/oz-tools/src/file_ops.rs:38), but every
 *  Tauri file command (`open_artifact`, `read_file_content`) canonicalises
 *  against the APP process cwd. A relative path handed straight to the IPC
 *  boundary therefore lands somewhere else — or nowhere. Resolve it here, on
 *  the side that still knows the working dir. */
export function resolveAgainstWorkingDir(p: string, workingDir: string): string {
  if (!p) return p;
  const normalized = normalizePath(p);
  if (normalized.startsWith("/")) return normalized;
  if (!workingDir) return normalized;
  return normalizePath(`${workingDir.replace(/\/+$/, "")}/${normalized}`);
}
