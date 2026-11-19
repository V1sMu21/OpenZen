// Approval store — manages the queue of pending tool approval requests.
// Shown one at a time via ApprovalModal.svelte.
//
// Svelte 5 writable stores only notify subscribers when the NEW state is
// not the same reference (safe_not_equal), so every update() here returns
// a fresh object instead of mutating in place — previously the countdown
// and modal visibility only updated when an unrelated re-render happened
// to pick up the mutation.

import { get, writable } from "svelte/store";
import { isTauri, tauriInvoke } from "../api/tauri";
import { getAuthToken } from "../api/chat";

export interface ApprovalRequest {
  requestId: string;
  sessionId: string;
  toolName: string;
  pattern: string;
  arguments: unknown;
  approvedCount: number;
  currentLevel: string;
  /** Set when the request came from a subagent (its label). */
  origin?: string;
}

const APPROVAL_TIMEOUT_SECS = 30 * 60;

export interface ApprovalState {
  queue: ApprovalRequest[];
  current: ApprovalRequest | null;
  showModal: boolean;
  countdown: number;
  /** Wall-clock deadline for the countdown. Tick-counting alone stalls when
   *  macOS throttles timers in an occluded window: the modal can sit for
   *  minutes with "30m" left and never auto-deny. */
  deadline: number;
  /** Set when the decision could not be delivered to the backend. The modal
   *  stays open (and the queue does NOT advance) so the user can retry —
   *  advancing would look like the tool was approved while the backend is
   *  still waiting, and the run would silently stall. */
  sendError: string | null;
}

function createApprovalStore() {
  const { subscribe, set, update } = writable<ApprovalState>({
    queue: [],
    current: null,
    showModal: false,
    countdown: APPROVAL_TIMEOUT_SECS,
    deadline: 0,
    sendError: null,
  });

  // The interval handle is UI-irrelevant bookkeeping — keep it out of the
  // reactive state so interval churn doesn't trigger subscriber noise.
  let countdownInterval: ReturnType<typeof setInterval> | null = null;
  // Last decision that failed to reach the backend, so "retry" re-sends the
  // SAME answer instead of guessing (a failed deny must not become an allow).
  let lastFailedDecision: string | null = null;
  let responding = false;

  function clearCountdownInterval() {
    if (countdownInterval) {
      clearInterval(countdownInterval);
      countdownInterval = null;
    }
  }

  function startCountdown(deadline: number = Date.now() + APPROVAL_TIMEOUT_SECS * 1000) {
    clearCountdownInterval();
    // Set the deadline in its own update; never call update() from inside
    // another update()'s updater (Svelte's set uses the pre-update snapshot,
    // which overwrote the deadline back to 0 and auto-denied in ~0.5s).
    update((s) => ({
      ...s,
      deadline,
      countdown: Math.max(0, Math.ceil((deadline - Date.now()) / 1000)),
    }));
    countdownInterval = setInterval(() => {
      update((s) => {
        // Compute from the wall clock, not from the number of ticks: a
        // throttled interval would otherwise never reach zero.
        const remaining = Math.max(0, Math.ceil((s.deadline - Date.now()) / 1000));
        if (remaining <= 0) {
          clearCountdownInterval();
          // Network call must run outside update(), so fire-and-forget it.
          if (s.current) {
            void respondInner("deny");
          }
          return { ...s, countdown: 0 };
        }
        return { ...s, countdown: remaining };
      });
    }, 500);
  }

  function stopCountdown() {
    clearCountdownInterval();
  }

  function push(request: ApprovalRequest) {
    const state = get({ subscribe });
    if (state.current?.requestId === request.requestId) return;
    if (state.queue.some((r) => r.requestId === request.requestId)) return;
    if (state.current) {
      update((s) => ({ ...s, queue: [...s.queue, request] }));
      return;
    }
    const deadline = Date.now() + APPROVAL_TIMEOUT_SECS * 1000;
    update((s) => ({
      ...s,
      current: request,
      showModal: true,
      countdown: APPROVAL_TIMEOUT_SECS,
      deadline,
      sendError: null,
    }));
    startCountdown(deadline);
  }

  async function respond(decision: string, requestId?: string) {
    stopCountdown();
    await respondInner(decision, requestId);
    // On failure the modal stays open with the error + Retry; the countdown
    // is deliberately NOT re-armed (a failing auto-deny every 30 min would just
    // spam the backend).
  }

  function advance() {
    const next = get({ subscribe }).queue[0] ?? null;
    const deadline = next ? Date.now() + APPROVAL_TIMEOUT_SECS * 1000 : 0;
    update((s) => ({
      ...s,
      queue: next ? s.queue.slice(1) : [],
      current: next,
      showModal: !!next,
      countdown: next ? APPROVAL_TIMEOUT_SECS : 0,
      deadline,
      sendError: null,
    }));
    if (next) startCountdown(deadline);
    else stopCountdown();
  }

  async function respondInner(decision: string, requestId?: string) {
    if (responding) return;
    const state = get({ subscribe });
    if (!state.current) return;
    if (requestId && state.current.requestId !== requestId) return;
    responding = true;
    const req = state.current;

    // Send response to backend (network call — must not run inside update())
    try {
      if (isTauri()) {
        await tauriInvoke("approve_tool", {
          sessionId: req.sessionId,
          requestId: req.requestId,
          decision,
        });
      } else {
        await fetch(`/api/sessions/${req.sessionId}/approve`, {
          method: "POST",
          headers: {
            "Content-Type": "application/json",
            Authorization: `Bearer ${getAuthToken()}`,
          },
          body: JSON.stringify({ request_id: req.requestId, decision }),
        });
      }
    } catch (err) {
      console.error("[approval] failed to send response:", err);
      const msg = err instanceof Error ? err.message : String(err);
      responding = false;
      // The backend timed out / was stopped first: this request can never be
      // answered. Drop it and advance, or the full-screen modal is stuck.
      if (/not found|unknown request|expired/i.test(msg)) {
        advance();
        return;
      }
      lastFailedDecision = decision;
      update((s) => ({ ...s, sendError: msg }));
      return;
    }

    lastFailedDecision = null;
    responding = false;
    advance();
  }

  /** Test-only reset; production never calls this. */
  function reset() {
    stopCountdown();
    responding = false;
    lastFailedDecision = null;
    set({ current: null, queue: [], showModal: false, countdown: 0, deadline: 0, sendError: null });
  }

  /** Re-send the decision that failed, without touching the queue. */
  async function retry() {
    if (!lastFailedDecision) return;
    await respondInner(lastFailedDecision);
  }

  return { subscribe, push, respond, retry, reset };
}

export const approval = createApprovalStore();
