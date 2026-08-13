<script lang="ts">
  import { onMount } from "svelte";
  import { Terminal } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import "@xterm/xterm/css/xterm.css";

  let containerEl: HTMLDivElement;
  let fallbackEl: HTMLPreElement;
  let term: Terminal | null = null;
  let fitAddon: FitAddon | null = null;
  let sessionId = $state<string | null>(null);
  let connected = $state(false);
  let unlisteners: (() => void)[] = [];
  let resizeObserver: ResizeObserver | null = null;
  let termFailed = $state(false);

  let { shell, cwd } = $props<{ shell?: string; cwd?: string }>();

  // True once the component has been unmounted; guards every continuation
  // after an `await` so a detached component never spawns a PTY, attaches
  // listeners, or mutates state after teardown.
  let disposed = false;
  // Plain (non-reactive) copy of the live session id, used by cleanup and
  // event filters. `sessionId` stays $state only for the template.
  let spawnedSessionId: string | null = null;

  onMount(() => {
    void initTerminal();
    // Synchronous cleanup: runs on unmount regardless of where initTerminal
    // is parked, so listeners/PTY can never outlive the component.
    return () => {
      disposed = true;
      if (spawnedSessionId) {
        invoke("close_terminal", { sessionId: spawnedSessionId }).catch(() => {});
      }
      term?.dispose();
      term = null;
      resizeObserver?.disconnect();
      unlisteners.forEach((u) => u());
      unlisteners = [];
    };
  });

  async function initTerminal() {
    const hasSize = () => {
      if (!containerEl) return false;
      const r = containerEl.getBoundingClientRect();
      return r.width > 0 && r.height > 0;
    };

    if (!hasSize()) {
      await new Promise<void>((resolve) => {
        const deadline = Date.now() + 10_000;
        const poll = () => {
          if (hasSize()) return resolve();
          if (Date.now() > deadline) return resolve();
          requestAnimationFrame(poll);
        };
        requestAnimationFrame(poll);
      });
    }
    if (disposed || !hasSize()) return;

    // Try xterm.js — if it throws or produces no visible output, fall
    // back to the plain <pre> element so the user always sees the shell.
    let xtermOk = false;
    try {
      const style = getComputedStyle(document.documentElement);
      const bg = style.getPropertyValue("--color-surface").trim() || "#080808";
      const fg = style.getPropertyValue("--color-ink").trim() || "#ede8df";
      const accent = style.getPropertyValue("--color-primary").trim() || "#81b5c7";

      term = new Terminal({
        cursorBlink: true,
        fontSize: 13,
        fontFamily: "Menlo, Monaco, 'Courier New', monospace",
        theme: {
          background: bg,
          foreground: fg,
          cursor: accent,
          selectionBackground: accent + "44",
        },
        rows: 24,
        cols: 80,
      });

      fitAddon = new FitAddon();
      term.loadAddon(fitAddon);
      term.open(containerEl);

      // Fit after open so cols/rows reflect real container size
      requestAnimationFrame(() => {
        fitAddon?.fit();
      });

      // Self-test: write a colored diagnostic line
      term.write("\x1b[2J\x1b[H\x1b[32m● Terminal ready\x1b[0m\r\n");

      // Verify the canvas/div was injected by xterm
      const xtermDiv = containerEl.querySelector(".xterm");
      if (xtermDiv) {
        xtermOk = true;
      }
    } catch (e) {
      console.warn("[ArtifactTerminal] xterm init failed:", e);
    }

    if (!xtermOk) {
      term = null;
      termFailed = true;
    }

    if (term) {
      term.onData((data) => {
        if (!spawnedSessionId) return;
        invoke("write_to_terminal", { sessionId: spawnedSessionId, data }).catch(() => {});
      });

      term.onResize(({ cols, rows }) => {
        if (!spawnedSessionId) return;
        invoke("resize_terminal", { sessionId: spawnedSessionId, cols, rows }).catch(() => {});
      });
    }

    resizeObserver = new ResizeObserver(() => {
      if (hasSize()) {
        fitAddon?.fit();
      }
    });
    resizeObserver.observe(containerEl);

    // Listeners registered BEFORE spawn
    const u1 = await listen<{ session_id: string; data: string }>(
      "terminal:data",
      (event) => {
        // Only render our own session's output — with several terminal
        // tabs open each tab receives every terminal's events. Before the
        // PTY exists (spawnedSessionId === null) there is nothing to mirror.
        if (spawnedSessionId === null || event.payload.session_id !== spawnedSessionId) return;
        if (term) {
          term.write(event.payload.data);
        }
        // Fallback: always mirror to <pre> so output is visible even if xterm fails
        if (fallbackEl) {
          fallbackEl.textContent += event.payload.data;
          fallbackEl.scrollTop = fallbackEl.scrollHeight;
        }
      },
    );
    unlisteners.push(u1);
    if (disposed) return;

    const u2 = await listen<{
      session_id: string;
      exit_code: number | null;
    }>("terminal:exited", (event) => {
      if (event.payload.session_id === spawnedSessionId) {
        connected = false;
        const code = event.payload.exit_code;
        const msg =
          code !== null
            ? `\r\n\r\n[Process exited with code ${code}]\r\n`
            : "\r\n\r\n[Process exited]\r\n";
        if (term) term.write(msg);
        if (fallbackEl) fallbackEl.textContent += msg;
      }
    });
    unlisteners.push(u2);
    if (disposed) return;

    let sid: string;
    try {
      sid = await invoke<string>("spawn_terminal", {
        shell: shell ?? null,
        cwd: cwd ?? null,
      });
    } catch (e) {
      console.warn("[ArtifactTerminal] spawn_terminal failed:", e);
      sessionId = null;
      return;
    }

    if (disposed) {
      // The tab was closed while the PTY was spawning — close it now so it
      // doesn't outlive the component.
      invoke("close_terminal", { sessionId: sid }).catch(() => {});
      return;
    }

    spawnedSessionId = sid;
    sessionId = sid;
    connected = true;
    fitAddon?.fit();

    const onResize = () => fitAddon?.fit();
    window.addEventListener("resize", onResize);
    unlisteners.push(() => window.removeEventListener("resize", onResize));
  }
</script>

<div class="terminal-wrapper">
  <!-- Fallback pre always visible — guaranteed to show PTY output -->
  <pre bind:this={fallbackEl} class="terminal-fallback"></pre>
  <!-- xterm overlay on top -->
  <div bind:this={containerEl} class="terminal-container"></div>
  {#if !connected}
    <div class="terminal-placeholder">
      {#if sessionId === null}
        <p>Failed to start terminal</p>
      {:else}
        <p>Connecting...</p>
      {/if}
    </div>
  {/if}
</div>

<style>
  .terminal-wrapper {
    height: 100%;
    position: relative;
    overflow: hidden;
  }
  .terminal-container {
    position: absolute;
    inset: 0;
    z-index: 2;
  }
  .terminal-fallback {
    position: absolute;
    inset: 0;
    margin: 0;
    padding: 4px 8px;
    background: var(--color-surface);
    color: var(--color-ink);
    font-family: Menlo, Monaco, 'Courier New', monospace;
    font-size: 13px;
    line-height: 1.3;
    overflow: auto;
    white-space: pre-wrap;
    z-index: 0;
  }
  .terminal-placeholder {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--text-muted, #888);
    font-size: 14px;
    z-index: 3;
  }
</style>
