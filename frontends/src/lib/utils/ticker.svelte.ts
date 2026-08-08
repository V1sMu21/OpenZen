// ── Global 1s ticker ──
// A single module-level setInterval ticks every 1000ms and writes to
// a shared $state proxy. Every ChatMessage instance reads from this
// proxy to compute its live elapsed time. This guarantees the timer
// ticks reliably even if a Svelte component is remounted mid-run
// (which would clear any interval set up in onMount). The interval
// is started once on first import and lives for the lifetime of
// the page.

import { onDestroy } from "svelte";

const TICK_MS = 1000;

// Module-level state: the current wall clock time.
export const tickerState = $state({ now: Date.now() });

// Module-level interval: started on first read, never stopped (the
// page lives as long as the ticker).
let intervalId: ReturnType<typeof setInterval> | null = null;

function ensureTickerRunning() {
  if (intervalId !== null) return;
  intervalId = setInterval(() => {
    tickerState.now = Date.now();
  }, TICK_MS);
}

// Called by components when they mount. Starts the ticker if not
// already running and returns a teardown that does NOT stop the
// ticker (since other components may still depend on it). Instead
// it just returns a no-op; the ticker runs for the page lifetime.
export function useTicker() {
  ensureTickerRunning();
  // Return a no-op cleanup; we don't stop the global ticker.
  return () => {};
}
