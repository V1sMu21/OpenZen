<script lang="ts">
  import ArtifactRenderer from "./ArtifactRenderer.svelte";
  import { sidepanel, type Artifact } from "../stores/sidepanel.svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { t } from "../i18n";

  // ── Detect artifact type from extension ──
  function detectType(path: string): string {
    const ext = path.split(".").pop()?.toLowerCase() ?? "";
    const map: Record<string, string> = {
      html: "html", htm: "html",
      pdf: "pdf",
      xlsx: "spreadsheet", xls: "spreadsheet", csv: "spreadsheet", tsv: "spreadsheet",
      py: "code", rs: "code", ts: "code", js: "code", go: "code",
      svelte: "code", json: "code", yaml: "code", yml: "code", toml: "code",
      sql: "code", sh: "code", css: "code", scss: "code", txt: "code",
      md: "markdown", rtf: "markdown",
      tex: "latex", lt: "latex", sty: "code", cls: "code", bib: "code",
      png: "image", jpg: "image", jpeg: "image", gif: "image", svg: "image", webp: "image",
      doc: "office", docx: "office", ppt: "office", pptx: "office",
    };
    return map[ext] ?? "code";
  }

  function rendererFor(artifact: Artifact): string {
    return artifact.type || detectType(artifact.path);
  }

  // ── Close single tab ──
  function handleTabClose(e: MouseEvent, id: string) {
    e.stopPropagation();
    sidepanel.closeTab(id);
  }

  // Only opens the artifact tab. ArtifactTerminal.svelte handles
  // the actual PTY spawn in its onMount — spawning here first would
  // create a wasted session whose output is never displayed.
  let spawning = false;
  async function spawnTerminal() {
    if (spawning) return; // debounce double-taps
    spawning = true;
    try {
      await sidepanel.open({
        type: "terminal",
        path: ".",
        label: "Terminal",
      });
    } catch (e) {
      console.warn("[SidePanel] open('terminal') failed:", e);
    } finally {
      spawning = false;
    }
  }

  // ── Open file via native dialog ──
  // The dialog now runs in Rust (`open_artifact_dialog`): the picked path
  // never crosses the webview boundary, so no JS can register arbitrary
  // files in the preview whitelist (P2-3).
  let openingFile = false;
  let fileError = $state<string | null>(null);
  async function openFile() {
    if (openingFile) return;
    openingFile = true;
    fileError = null;
    try {
      await invoke<Artifact>("open_artifact_dialog");
    } catch (e) {
      if (String(e) !== "cancelled") fileError = String(e);
    } finally {
      openingFile = false;
    }
  }

  // ── Drag resize (Pointer Events + capture so mousemove keeps flowing
  //    even when the cursor is over the preview iframe) ──
  let dragging = $state(false);
  let dragPointerId = $state<number | null>(null);
  let startX = $state(0);
  let startWidth = $state(0);

  function onDragStart(e: PointerEvent) {
    e.preventDefault();
    const handle = e.currentTarget as HTMLElement;
    handle.setPointerCapture(e.pointerId);
    dragPointerId = e.pointerId;
    dragging = true;
    startX = e.clientX;
    startWidth = sidepanel.width;
    document.body.style.userSelect = "none";
    document.body.style.cursor = "col-resize";
  }

  function onDragMove(e: PointerEvent) {
    if (!dragging || e.pointerId !== dragPointerId) return;
    e.preventDefault();
    const delta = startX - e.clientX;
    sidepanel.width = Math.max(280, Math.min(800, startWidth + delta));
  }

  function onDragEnd(e: PointerEvent) {
    if (e.pointerId !== dragPointerId) return;
    dragging = false;
    dragPointerId = null;
    document.body.style.userSelect = "";
    document.body.style.cursor = "";
    const handle = e.currentTarget as HTMLElement;
    if (handle.hasPointerCapture(e.pointerId)) {
      handle.releasePointerCapture(e.pointerId);
    }
    sidepanel.setWidth(sidepanel.width);
  }

  function onDragCancel(e: PointerEvent) {
    if (e.pointerId !== dragPointerId) return;
    dragging = false;
    dragPointerId = null;
    document.body.style.userSelect = "";
    document.body.style.cursor = "";
    sidepanel.setWidth(sidepanel.width);
  }
</script>

{#if sidepanel.visible}
  <aside class="sidepanel" class:dragging={dragging} style="width:{sidepanel.width}px;">
    <!-- Drag handle -->
    <div
      class="sidepanel-resize-handle"
      class:dragging={dragging}
      onpointerdown={onDragStart}
      onpointermove={onDragMove}
      onpointerup={onDragEnd}
      onpointercancel={onDragCancel}
      onlostpointercapture={onDragCancel}
      role="separator"
    ></div>

    <!-- Header: close button + tabs -->
    <div class="sidepanel-header">
      <div class="sidepanel-tabbar">
      {#each sidepanel.artifacts as a (a.id)}
        <button
          class="sidepanel-tab"
          class:active={a.id === sidepanel.activeId}
          onclick={() => sidepanel.selectTab(a.id)}
          title={a.path}
        >
          <span class="tab-label">{a.label}</span>
          <span
            class="tab-close"
            onclick={(e) => handleTabClose(e, a.id)}
            aria-label={$t("sidepanel.closeTab")}
          >✕</span>
        </button>
      {/each}
      <button
        class="sidepanel-add-btn"
        onclick={spawnTerminal}
        title={$t("sidepanel.newTerminal")}
      >{$t("sidepanel.terminal")}</button>
      <button
        class="sidepanel-add-btn"
        onclick={openFile}
        title={$t("sidepanel.openFile")}
      >{$t("sidepanel.open")}</button>
      <span class="sidepanel-header-spacer"></span>
      <button class="sidepanel-close-btn" onclick={() => sidepanel.close()} title={$t("sidepanel.close")}>✕</button>
    </div>
    </div>

    {#if fileError}
      <div class="sidepanel-error" role="alert">
        {fileError}
        <button class="sidepanel-error-dismiss" onclick={() => fileError = null}>✕</button>
      </div>
    {/if}

    <!-- Content area -->
    <div class="sidepanel-content">
      {#if sidepanel.activeArtifact}
        <!-- {#key} remounts the view whenever the active artifact changes.
             Every Artifact view loads only in onMount, so without the key
             switching between same-type tabs kept showing the previous
             file (and terminal tabs shared one PTY). -->
        {#key sidepanel.activeArtifact.id}
          {@const renderer = rendererFor(sidepanel.activeArtifact)}
          <ArtifactRenderer {renderer} artifact={sidepanel.activeArtifact} />
        {/key}
      {:else}
        <ArtifactEmpty />
      {/if}
    </div>
  </aside>
{/if}

<style>
  .sidepanel {
    position: relative;
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--color-surface-elevated);
    border-left: 1px solid var(--color-hairline);
    overflow: hidden;
    transition: width 250ms ease-out;
  }
  .sidepanel.dragging {
    transition: none;
  }

  .sidepanel-resize-handle {
    position: absolute;
    left: 0;
    top: 0;
    bottom: 0;
    width: 12px;
    cursor: col-resize;
    z-index: 20;
    user-select: none;
    touch-action: none;
  }
  .sidepanel-resize-handle::after {
    content: "";
    position: absolute;
    left: 4px;
    top: 0;
    bottom: 0;
    width: 3px;
    background: transparent;
    transition: background 0.2s ease;
  }
  .sidepanel-resize-handle:hover::after,
  .sidepanel-resize-handle.dragging::after {
    background: var(--color-primary);
  }

  .sidepanel-header {
    display: flex;
    align-items: center;
    border-bottom: 1px solid var(--color-hairline);
    flex-shrink: 0;
    min-height: 36px;
  }
  .sidepanel-header-spacer { flex: 1; }
  .sidepanel-close-btn {
    background: none;
    border: none;
    color: var(--color-muted);
    cursor: pointer;
    font-size: 14px;
    padding: 4px 10px;
    border-radius: 4px;
    flex-shrink: 0;
  }
  .sidepanel-close-btn:hover {
    color: var(--color-ink);
    background: var(--color-surface-elevated);
  }

  .sidepanel-tabbar {
    display: flex;
    overflow-x: auto;
    flex-shrink: 0;
    padding: 4px 4px 0 8px;
    gap: 2px;
  }

  .sidepanel-tab {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 6px 10px;
    border: none;
    border-bottom: 2px solid transparent;
    background: transparent;
    color: var(--color-muted);
    cursor: pointer;
    font-family: var(--font-serif);
    font-size: 12px;
    letter-spacing: 0.08em;
    border-radius: 3px 3px 0 0;
    white-space: nowrap;
    max-width: 160px;
    overflow: hidden;
    transition: color 0.3s var(--ease-soak, ease), border-color 0.3s var(--ease-soak, ease);
  }
  .sidepanel-tab.active {
    background: transparent;
    color: var(--color-primary);
    border-bottom-color: var(--color-primary);
  }
  .sidepanel-tab:hover {
    color: var(--color-ink);
  }

  .tab-label {
    overflow: hidden;
    text-overflow: ellipsis;
    flex: 1;
  }

  .tab-close {
    font-size: 10px;
    opacity: 0.5;
    padding: 2px;
    border-radius: 2px;
  }
  .tab-close:hover {
    opacity: 1;
    background: var(--color-error);
    color: white;
  }

  .sidepanel-add-btn {
    margin-left: 4px;
    padding: 4px 8px;
    font-size: 11px;
    border: 1px dashed var(--color-hairline);
    border-radius: 4px;
    background: transparent;
    color: var(--color-muted);
    cursor: pointer;
    white-space: nowrap;
  }
  .sidepanel-add-btn:hover {
    border-color: var(--color-primary);
    color: var(--color-primary);
  }

  .sidepanel-content {
    flex: 1;
    overflow: auto;
    position: relative;
  }

  .sidepanel-error {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 6px 10px;
    background: rgba(196, 77, 77, 0.12);
    border-bottom: 1px solid var(--color-error);
    color: var(--color-error);
    font-size: 12px;
    flex-shrink: 0;
  }
  .sidepanel-error-dismiss {
    background: none;
    border: none;
    color: var(--color-error);
    cursor: pointer;
    font-size: 12px;
    padding: 0 4px;
    flex-shrink: 0;
  }

  .sidepanel-placeholder {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100%;
    color: var(--color-body);
    font-size: 14px;
  }
  .sidepanel-placeholder .hint {
    font-size: 12px;
    opacity: 0.7;
    margin-top: 4px;
    color: var(--color-muted);
  }
</style>
