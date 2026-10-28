// Live "running / waiting for user" session sets — power the sidebar
// indicator so the user can tell at a glance which sessions are mid-task
// and which ones are parked on a question.
//
// Maintained from two sources:
//  1. The global SSE stream: sse.ts feeds every session's lifecycle events
//     here BEFORE its per-session filtering (background sessions included).
//     `model_info` fires at run start, `ask_user_pending` parks it,
//     `ask_user_answered` resumes it, and `done`/`error` ends it.
//  2. list_sessions results: the backend reports `running` / `waiting`, so
//     reseeding after a full load corrects any drift (missed events, app
//     reload while runs continue backend-side).
//
// Notifications are content-compared: RUN_START events include per-token
// streaming events, and Svelte treats any object as changed
// (safe_not_equal), so publishing a new Set on every token would re-render
// every sidebar row continuously. publish() only notifies on real changes.

import { writable } from "svelte/store";

let runningCurrent: ReadonlySet<string> = new Set();
let waitingCurrent: ReadonlySet<string> = new Set();
const runningIds = writable<ReadonlySet<string>>(runningCurrent);
const waitingIds = writable<ReadonlySet<string>>(waitingCurrent);

export const runningSessionIds = { subscribe: runningIds.subscribe };
export const waitingSessionIds = { subscribe: waitingIds.subscribe };

function sameSet(a: ReadonlySet<string>, b: ReadonlySet<string>): boolean {
  if (a.size !== b.size) return false;
  for (const v of b) {
    if (!a.has(v)) return false;
  }
  return true;
}

function publish(
  nextRunning: ReadonlySet<string>,
  nextWaiting: ReadonlySet<string>,
): void {
  if (!sameSet(runningCurrent, nextRunning)) {
    runningCurrent = nextRunning;
    runningIds.set(nextRunning);
  }
  if (!sameSet(waitingCurrent, nextWaiting)) {
    waitingCurrent = nextWaiting;
    waitingIds.set(nextWaiting);
  }
}

export function markSessionRunning(sessionId: string | undefined): void {
  if (!sessionId) return;
  if (runningCurrent.has(sessionId) && !waitingCurrent.has(sessionId)) return;
  const nextRunning = new Set(runningCurrent);
  const nextWaiting = new Set(waitingCurrent);
  nextRunning.add(sessionId);
  nextWaiting.delete(sessionId);
  publish(nextRunning, nextWaiting);
}

/** The run asked a question and is parked until the user answers. */
export function markSessionWaiting(sessionId: string | undefined): void {
  if (!sessionId) return;
  if (waitingCurrent.has(sessionId) && !runningCurrent.has(sessionId)) return;
  const nextRunning = new Set(runningCurrent);
  const nextWaiting = new Set(waitingCurrent);
  nextRunning.delete(sessionId);
  nextWaiting.add(sessionId);
  publish(nextRunning, nextWaiting);
}

export function markSessionFinished(sessionId: string | undefined): void {
  if (!sessionId) return;
  if (!runningCurrent.has(sessionId) && !waitingCurrent.has(sessionId)) return;
  const nextRunning = new Set(runningCurrent);
  const nextWaiting = new Set(waitingCurrent);
  nextRunning.delete(sessionId);
  nextWaiting.delete(sessionId);
  publish(nextRunning, nextWaiting);
}

/** Replace both sets from a full list_sessions payload (source of truth). */
export function seedRunningSessions(
  list: Array<{ id: string; status: string }>,
): void {
  publish(
    new Set(list.filter((s) => /^running$/i.test(s.status)).map((s) => s.id)),
    new Set(list.filter((s) => /^waiting$/i.test(s.status)).map((s) => s.id)),
  );
}
