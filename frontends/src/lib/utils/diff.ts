// Line-level diff of two text snippets.
//
// Moved verbatim out of EditCard.svelte: the changed-files card needs the same
// added/removed counts, and EditCard's rendering must not drift from them.
//
// Scope: these snippets are the tool call's `old_string`/`new_string`, i.e. a
// fragment of the file, not the whole file. The counts are therefore local to
// the edit — which is exactly what the `+N/-M` badge has always meant here.

export type DiffLineType = "removed" | "added" | "context";

export interface DiffLine {
  oldNum: number | null;
  newNum: number | null;
  text: string;
  type: DiffLineType;
}

export interface DiffStat {
  added: number;
  removed: number;
}

/** Diff two line arrays.
 *
 *  Full LCS only for small inputs; the guard keeps a pathological pair from
 *  blowing up the O(m*n) table. Larger inputs fall back to "all old removed,
 *  all new added", which stays linear and is honest for an edit whose two
 *  sides share almost nothing. */
export function computeDiff(oldLines: string[], newLines: string[]): DiffLine[] {
  const m = oldLines.length;
  const n = newLines.length;
  if (m === 0 && n === 0) return [];

  // For long files we cap the diff to avoid O(m*n) blow-up.
  const maxDiff = 2000;

  // Use a simplified patience-like diff: compare line-by-line.
  // For small diffs (common case) do full LCS.
  const useFullLcs = m * n <= maxDiff * 2;

  if (useFullLcs && m <= 500 && n <= 500) {
    return lcsDiff(oldLines, newLines);
  }

  return simpleDiff(oldLines, newLines);
}

function lcsDiff(oldLines: string[], newLines: string[]): DiffLine[] {
  const m = oldLines.length;
  const n = newLines.length;

  // Build LCS table
  const dp: number[][] = Array.from({ length: m + 1 }, () => Array(n + 1).fill(0));
  for (let i = 1; i <= m; i++) {
    for (let j = 1; j <= n; j++) {
      if (oldLines[i - 1] === newLines[j - 1]) {
        dp[i][j] = dp[i - 1][j - 1] + 1;
      } else {
        dp[i][j] = Math.max(dp[i - 1][j], dp[i][j - 1]);
      }
    }
  }

  // Backtrack to produce diff
  const result: DiffLine[] = [];

  let i = m;
  let j = n;
  while (i > 0 || j > 0) {
    if (i > 0 && j > 0 && oldLines[i - 1] === newLines[j - 1]) {
      result.unshift({ oldNum: i, newNum: j, text: oldLines[i - 1], type: "context" });
      i--;
      j--;
    } else if (j > 0 && (i === 0 || dp[i][j - 1] >= dp[i - 1][j])) {
      result.unshift({ oldNum: null, newNum: j, text: newLines[j - 1], type: "added" });
      j--;
    } else {
      result.unshift({ oldNum: i, newNum: null, text: oldLines[i - 1], type: "removed" });
      i--;
    }
  }
  return result;
}

function simpleDiff(oldLines: string[], newLines: string[]): DiffLine[] {
  const result: DiffLine[] = [];
  for (let i = 0; i < oldLines.length; i++) {
    result.push({ oldNum: i + 1, newNum: null, text: oldLines[i], type: "removed" });
  }
  for (let i = 0; i < newLines.length; i++) {
    result.push({ oldNum: null, newNum: i + 1, text: newLines[i], type: "added" });
  }
  return result;
}

/** Added/removed counts in one pass over a diff. */
export function diffStat(lines: DiffLine[]): DiffStat {
  let added = 0;
  let removed = 0;
  for (const l of lines) {
    if (l.type === "added") added++;
    else if (l.type === "removed") removed++;
  }
  return { added, removed };
}
