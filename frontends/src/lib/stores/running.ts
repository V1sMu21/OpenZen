// Live "running sessions" set — powers the sidebar's pulsing indicator so
// the user can spot which sessions are mid-task at a glance.
//
// Maintained from two sources:
//  1. The global SSE stream: sse.ts feeds every session's lifecycle events
//     here BEFORE its per-session filtering (background sessions included).
//     `model_info` fires at run start; `done`/`error` at run end.
//  2. list_sessions results: the backend clears "running" status for
//     sessions without a live agent, so reseeding after a full load corrects
//     any drift (missed events, app reload while runs continue backend-side).
//
// Notifications are content-compared: RUN_START events include per-token
// streaming events, and Svelte treats any object as changed
// (safe_not_equal), so publishing a new Set on every token would re-render
// every sidebar row continuously. publish() only notifies on real changes.

import { writable } from "svelte/store";

let current: ReadonlySet<string> = new Set();
const ids = writable<ReadonlySet<string>>(current);

export const runningSessionIds = { subscribe: ids.subscribe };

function sameSet(a: ReadonlySet<string>, b: ReadonlySet<string>): boolean {
  if (a.size !== b.size) return false;
  for (const v of b) {
    if (!a.has(v)) return false;
  }
  return true;
}

function publish(next: ReadonlySet<string>): void {
  if (sameSet(current, next)) return;
  current = next;
  ids.set(next);
}

export function markSessionRunning(sessionId: string | undefined): void {
  if (!sessionId || current.has(sessionId)) return;
  const next = new Set(current);
  next.add(sessionId);
  publish(next);
}

export function markSessionFinished(sessionId: string | undefined): void {
  if (!sessionId || !current.has(sessionId)) return;
  const next = new Set(current);
  next.delete(sessionId);
  publish(next);
}

/** Replace the set from a full list_sessions payload (source of truth). */
export function seedRunningSessions(
  list: Array<{ id: string; status: string }>,
): void {
  publish(
    new Set(list.filter((s) => /^running$/i.test(s.status)).map((s) => s.id)),
  );
}
