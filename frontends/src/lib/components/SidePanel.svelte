<script lang="ts">
  import ArtifactEmpty from "./ArtifactEmpty.svelte";
  import ArtifactRenderer from "./ArtifactRenderer.svelte";
  import { sidepanel, type Artifact } from "../stores/sidepanel.svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { t } from "../i18n";
  import { detectArtifactType } from "../utils/artifactType";
  import { isTauri } from "../api/tauri";

  function rendererFor(artifact: Artifact): string {
    return artifact.type || detectArtifactType(artifact.path);
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

  // ── Path bar / footer actions ──
  async function copyPath() {
    const path = sidepanel.activeArtifact?.path;
    if (!path) return;
    try {
      await navigator.clipboard.writeText(path);
    } catch {
      /* clipboard unavailable (webview without permission) — no-op */
    }
  }

  async function revealActive() {
    const path = sidepanel.activeArtifact?.path;
    if (!path || !isTauri()) return;
    try {
      await invoke("reveal_in_finder", { path });
    } catch (e) {
      console.warn("[SidePanel] reveal failed:", e);
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
    sidepanel.width = Math.max(280, Math.min(960, startWidth + delta));
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

    {#if sidepanel.activeArtifact && sidepanel.activeArtifact.type !== "background"}
      <div class="sidepanel-path">
        <span class="sp-path-text" title={sidepanel.activeArtifact.path}>{sidepanel.activeArtifact.path}</span>
        <button class="sp-mini" onclick={copyPath} title={$t("sidepanel.copyPath")} aria-label={$t("sidepanel.copyPath")}>⧉</button>
      </div>
    {/if}

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
             file (and terminal tabs shared one PTY).
             `openSeq` is part of the key because a file already on the tab
             bar is focused rather than re-added (one tab per path), leaving
             its id unchanged — without the token a re-opened file would keep
             rendering whatever its viewer read the first time. -->
        {#key sidepanel.activeArtifact.id + ":" + sidepanel.openSeq}
          {@const renderer = rendererFor(sidepanel.activeArtifact)}
          <ArtifactRenderer {renderer} artifact={sidepanel.activeArtifact} />
        {/key}
      {:else}
        <ArtifactEmpty />
      {/if}
    </div>

    <div class="sidepanel-foot">
      <span class="sp-meta">{sidepanel.artifacts.length} {$t("sidepanel.tabsCount")}</span>
      <span class="sp-grow"></span>
      {#if sidepanel.activeArtifact?.type !== "background"}
        <button class="sp-btn" onclick={revealActive}>{$t("sidepanel.reveal")}</button>
        <button class="sp-btn" onclick={copyPath}>{$t("sidepanel.copyPath")}</button>
      {/if}
    </div>
  </aside>
{/if}

<style>
  /* ── 右侧栏 (展卷): 56px 标签行 + 30px 路径条 + 44px 底栏 ── */
  .sidepanel {
    position: relative;
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--color-surface);
    border-left: 1px solid var(--color-hairline);
    overflow: hidden;
    transition: width 250ms var(--ease-soak);
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
    height: 56px;
    padding: 0 8px 0 12px;
  }
  .sidepanel-header-spacer { flex: 1; }
  .sidepanel-close-btn {
    background: none;
    border: none;
    color: var(--color-muted);
    cursor: pointer;
    font-size: 13px;
    width: 28px;
    height: 28px;
    border-radius: 8px;
    flex-shrink: 0;
    transition: background 0.16s, color 0.16s;
  }
  .sidepanel-close-btn:hover {
    color: var(--color-ink);
    background: var(--color-surface-elevated);
  }

  .sidepanel-tabbar {
    display: flex;
    align-items: center;
    overflow-x: auto;
    min-width: 0;
    flex-shrink: 1;
    gap: 4px;
  }

  .sidepanel-tab {
    display: flex;
    align-items: center;
    gap: 6px;
    height: 30px;
    padding: 0 9px;
    border: 1px solid transparent;
    background: transparent;
    color: var(--color-muted);
    cursor: pointer;
    font-family: var(--font-sans);
    font-size: 11.5px;
    border-radius: 9px;
    white-space: nowrap;
    max-width: 190px;
    flex: none;
    overflow: hidden;
    transition: color 0.18s var(--ease-soak), background 0.18s, border-color 0.18s;
  }
  .sidepanel-tab.active {
    background: var(--color-surface-soft);
    border-color: var(--color-hairline);
    color: var(--color-ink);
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

  /* 文字按钮（终端 / 打开）：宽度跟随文字，中英文都不重叠 */
  .sidepanel-add-btn {
    margin-left: 2px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    height: 28px;
    padding: 0 9px;
    font-family: var(--font-sans);
    font-size: 11px;
    line-height: 1;
    border: 1px solid var(--color-hairline);
    border-radius: 8px;
    background: transparent;
    color: var(--color-dim);
    cursor: pointer;
    white-space: nowrap;
    flex: none;
    transition: background 0.16s, color 0.16s, border-color 0.16s;
  }
  .sidepanel-add-btn:hover {
    background: var(--color-surface-soft);
    color: var(--color-ink);
    border-color: var(--color-hairline-strong);
  }
  .sidepanel-add-btn:hover {
    border-color: var(--color-primary);
    color: var(--color-primary);
  }

  .sidepanel-path {
    height: 30px;
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 12px;
    border-bottom: 1px solid var(--color-hairline);
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--color-muted);
  }
  .sp-path-text {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sp-mini {
    border: none;
    background: none;
    color: var(--color-muted);
    cursor: pointer;
    font-size: 11px;
    padding: 2px 4px;
    border-radius: 5px;
  }
  .sp-mini:hover { color: var(--color-ink); background: var(--color-surface-soft); }

  .sidepanel-foot {
    height: 44px;
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 12px;
    border-top: 1px solid var(--color-hairline);
    background: var(--color-surface);
  }
  .sp-grow { flex: 1; }
  .sp-meta {
    font-family: var(--font-mono);
    font-size: 9.5px;
    color: var(--color-muted);
  }
  .sp-btn {
    height: 26px;
    padding: 0 10px;
    border-radius: 8px;
    border: 1px solid var(--color-hairline);
    background: transparent;
    color: var(--color-dim);
    font-family: var(--font-sans);
    font-size: 10.5px;
    cursor: pointer;
    transition: background 0.16s, color 0.16s, border-color 0.16s;
  }
  .sp-btn:hover {
    background: var(--color-surface-soft);
    color: var(--color-ink);
    border-color: var(--color-hairline-strong);
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

</style>
