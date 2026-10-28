// Plan review store — keeps the list of plans and a pending count.
//
// The side panel owns the detail view (`PlanReview.svelte`); this store only
// answers "is there anything waiting for me?" and mirrors `plan:changed` so a
// rejection/approval anywhere refreshes the list.

import { listen } from "@tauri-apps/api/event";
import { listPlans, openPlan, type Plan } from "../api/plans";

function createPlansStore() {
  let items = $state<Plan[]>([]);
  let loaded = $state(false);
  let unlisteners: Array<() => void> = [];

  async function refresh() {
    try {
      items = await listPlans();
    } catch (_e) {
      // Tauri unavailable (dev server) — keep whatever we had.
    } finally {
      loaded = true;
    }
  }

  async function init() {
    await refresh();
    // A pending plan must not be invisible just because the app was restarted
    // with its tab closed: focus the newest one.
    const waiting = items.find((p) => p.status === "pending");
    if (waiting) {
      try {
        await openPlan(waiting.id);
      } catch (_e) {
        // Not running inside Tauri.
      }
    }
    try {
      const u1 = await listen<Plan>("plan:changed", () => {
        void refresh();
      });
      const u2 = await listen<Plan>("plan:decided", (event) => {
        const decided = event.payload;
        items = items.map((p) => (p.id === decided.id ? { ...p, ...decided } : p));
        void refresh();
      });
      unlisteners.push(u1, u2);
    } catch (_e) {
      // Not running inside Tauri.
    }
  }

  function dispose() {
    unlisteners.forEach((u) => u());
    unlisteners = [];
  }

  return {
    get items() {
      return items;
    },
    get loaded() {
      return loaded;
    },
    get pending() {
      return items.filter((p) => p.status === "pending").length;
    },
    refresh,
    init,
    dispose,
  };
}

export const plans = createPlansStore();
