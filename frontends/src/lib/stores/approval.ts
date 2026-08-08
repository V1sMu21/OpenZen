// Approval store — manages the queue of pending tool approval requests.
// Shown one at a time via ApprovalModal.svelte.

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
  countdownInterval: ReturnType<typeof setInterval> | null;
}

function createApprovalStore() {
  const { subscribe, set, update } = writable<ApprovalState>({
    queue: [],
    current: null,
    showModal: false,
    countdown: 30,
    countdownInterval: null,
  });

  function push(request: ApprovalRequest) {
    update((s) => {
      s.queue.push(request);
      if (!s.current) {
        s.current = s.queue.shift()!;
        s.showModal = true;
        s.countdown = 30;
        startCountdown();
      }
      return s;
    });
  }

  function startCountdown() {
    update((s) => {
      if (s.countdownInterval) clearInterval(s.countdownInterval);
      s.countdownInterval = setInterval(() => {
        update((inner) => {
          inner.countdown -= 1;
          if (inner.countdown <= 0) {
            if (inner.countdownInterval) clearInterval(inner.countdownInterval);
            inner.countdownInterval = null;
            // Auto-deny on timeout
            const current = inner.current;
            if (current) {
              respondInner("deny");
            }
          }
          return inner;
        });
      }, 1000);
      return s;
    });
  }

  function stopCountdown() {
    update((s) => {
      if (s.countdownInterval) clearInterval(s.countdownInterval);
      s.countdownInterval = null;
      return s;
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
      s.current = s.queue.shift() || null;
      if (s.current) {
        s.countdown = 30;
        startCountdown();
      } else {
        s.showModal = false;
      }
      return s;
    });
  }

  return { subscribe, push, respond };
}

export const approval = createApprovalStore();
