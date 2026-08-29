// Shared soul-status store — Svelte 5 $state rune.
//
// One source of truth for the three consumers (window title bar, SoulCard
// rail, settings panel soul tab), so a rename updates every consumer
// instantly instead of waiting for each one's own poll/remount. Writes go
// through `set`/`load`; both are safe after unmount (module-level state).

import { fetchSoulStatus, type SoulStatus } from "../api/settings";

function createSoulStore() {
  let status = $state<SoulStatus | null>(null);

  return {
    get status() {
      return status;
    },
    set(next: SoulStatus | null) {
      status = next;
    },
    async load() {
      try {
        status = await fetchSoulStatus();
      } catch {
        // webui mode or IPC failure — keep the previous snapshot
      }
    },
  };
}

export const soulStore = createSoulStore();
