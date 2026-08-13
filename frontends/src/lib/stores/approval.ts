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
}

export interface ApprovalState {
  queue: ApprovalRequest[];
  current: ApprovalRequest | null;
  showModal: boolean;
  countdown: number;
}

function createApprovalStore() {
  const { subscribe, set, update } = writable<ApprovalState>({
    queue: [],
    current: null,
    showModal: false,
    countdown: 30,
  });

  // The interval handle is UI-irrelevant bookkeeping — keep it out of the
  // reactive state so interval churn doesn't trigger subscriber noise.
  let countdownInterval: ReturnType<typeof setInterval> | null = null;

  function clearCountdownInterval() {
    if (countdownInterval) {
      clearInterval(countdownInterval);
      countdownInterval = null;
    }
  }

  function startCountdown() {
    clearCountdownInterval();
    countdownInterval = setInterval(() => {
      update((s) => {
        const countdown = s.countdown - 1;
        if (countdown <= 0) {
          clearCountdownInterval();
          // Auto-deny on timeout. The network call must run outside
          // update(), so fire-and-forget it.
          if (s.current) {
            void respondInner("deny");
          }
          return { ...s, countdown: 0 };
        }
        return { ...s, countdown };
      });
    }, 1000);
  }

  function stopCountdown() {
    clearCountdownInterval();
  }

  function push(request: ApprovalRequest) {
    update((s) => {
      if (s.current) {
        return { ...s, queue: [...s.queue, request] };
      }
      const current = request;
      startCountdown();
      return {
        ...s,
        queue: s.queue,
        current,
        showModal: true,
        countdown: 30,
      };
    });
  }

  async function respond(decision: string) {
    stopCountdown();
    await respondInner(decision);
  }

  async function respondInner(decision: string) {
    const state = get({ subscribe });
    if (!state.current) return;
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
    }

    // Advance queue
    update((s) => {
      const current = s.queue[0] ?? null;
      if (current) {
        startCountdown();
        return {
          ...s,
          queue: s.queue.slice(1),
          current,
          countdown: 30,
        };
      }
      return { ...s, queue: [], current: null, showModal: false, countdown: 0 };
    });
  }

  return { subscribe, push, respond };
}

export const approval = createApprovalStore();
