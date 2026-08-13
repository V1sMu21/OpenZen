<script lang="ts">
  import { onMount } from "svelte";
  import * as pdfjsLib from "pdfjs-dist";
  import { invoke } from "@tauri-apps/api/core";
  import type { Artifact } from "../stores/sidepanel";
  import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";

  pdfjsLib.GlobalWorkerOptions.workerSrc = workerUrl;

  let { artifact } = $props<{ artifact: Artifact }>();

  let canvasEl: HTMLCanvasElement;
  let pageNum = $state(1);
  let numPages = $state(0);
  let scale = $state(1.2);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let pdfDoc: pdfjsLib.PDFDocumentProxy | null = null;
  let loadingTask: pdfjsLib.PDFDocumentLoadingTask | null = null;
  let disposed = false;
  // Monotonic token: rapid page/zoom changes supersede in-flight renders so
  // a stale render can never paint over the current page.
  let renderToken = 0;

  async function loadPDF() {
    try {
      const bytes: number[] = await invoke("read_file_bytes", { path: artifact.path });
      if (disposed) return;
      loadingTask = pdfjsLib.getDocument({ data: new Uint8Array(bytes) });
      pdfDoc = await loadingTask.promise;
      if (disposed) return;
      numPages = pdfDoc.numPages;
      loading = false;
      renderPage();
    } catch (e) {
      if (disposed) return;
      error = String(e);
      loading = false;
    }
  }

  async function renderPage() {
    if (!canvasEl || !pdfDoc) return;
    const myToken = ++renderToken;
    loading = true;
    try {
      const page = await pdfDoc.getPage(pageNum);
      if (disposed || myToken !== renderToken) { page.cleanup(); return; }
      const viewport = page.getViewport({ scale });
      canvasEl.height = viewport.height;
      canvasEl.width = viewport.width;
      await page.render({ canvas: canvasEl, viewport }).promise;
      // Release page-owned resources (fonts, worker objects) immediately.
      page.cleanup();
      if (!disposed && myToken === renderToken) loading = false;
    } catch (e) {
      // Rendering against a destroyed doc throws — expected during teardown
      // or when a newer render superseded this one.
      if (!disposed && myToken === renderToken) {
        error = String(e);
      }
    }
  }

  async function goToPage(n: number) {
    pageNum = n;
    await renderPage();
  }
  function prevPage() { if (pageNum > 1) goToPage(pageNum - 1); }
  function nextPage() { if (pageNum < numPages) goToPage(pageNum + 1); }
  function zoomIn() { scale = Math.min(scale + 0.2, 3); renderPage(); }
  function zoomOut() { scale = Math.max(scale - 0.2, 0.4); renderPage(); }

  function handleKey(e: KeyboardEvent) {
    if (e.key === "ArrowLeft") { e.preventDefault(); prevPage(); }
    if (e.key === "ArrowRight") { e.preventDefault(); nextPage(); }
  }

  onMount(() => {
    loadPDF();
    window.addEventListener("keydown", handleKey);
    return () => {
      disposed = true;
      renderToken++;
      window.removeEventListener("keydown", handleKey);
      // Destroy the loading task (document + worker) when the tab closes —
      // the previous code leaked both on every artifact switch. In
      // pdfjs-dist v6 cleanup goes through the loading task; the proxy no
      // longer exposes a public destroy().
      loadingTask?.destroy().catch(() => {});
      pdfDoc = null;
    };
  });
</script>

<div class="pdf-viewer">
  {#if error}
    <div class="pdf-error">{error}</div>
  {:else}
    <div class="pdf-toolbar">
      <button onclick={prevPage} disabled={pageNum <= 1}>◀</button>
      <span class="pdf-page-info">{pageNum} / {numPages}</span>
      <button onclick={nextPage} disabled={pageNum >= numPages}>▶</button>
      <span class="pdf-spacer"></span>
      <button onclick={zoomOut}>−</button>
      <span class="pdf-zoom">{Math.round(scale * 100)}%</span>
      <button onclick={zoomIn}>+</button>
    </div>
    <div class="pdf-canvas-wrap">
      {#if loading}
        <div class="pdf-loading">Loading...</div>
      {/if}
      <canvas bind:this={canvasEl}></canvas>
    </div>
  {/if}
</div>

<style>
  .pdf-viewer { display: flex; flex-direction: column; height: 100%; }
  .pdf-toolbar {
    display: flex; align-items: center; gap: 8px; padding: 6px 8px;
    background: var(--bg-surface, #1a1a2e); border-bottom: 1px solid var(--border-color, #333);
    flex-shrink: 0; font-size: 12px; color: var(--text-muted, #888);
  }
  .pdf-toolbar button {
    background: none; border: 1px solid var(--border-color, #444); border-radius: 4px;
    padding: 2px 8px; color: var(--text-muted, #ccc); cursor: pointer; font-size: 12px;
  }
  .pdf-toolbar button:disabled { opacity: 0.3; cursor: default; }
  .pdf-toolbar button:hover:not(:disabled) { background: var(--bg-hover, #2a2a4e); }
  .pdf-spacer { flex: 1; }
  .pdf-page-info, .pdf-zoom { min-width: 50px; text-align: center; }
  .pdf-canvas-wrap { flex: 1; overflow: auto; display: flex; justify-content: center; }
  .pdf-loading { position: absolute; color: var(--text-muted, #888); padding: 20px; }
  .pdf-error { display: flex; align-items: center; justify-content: center;
    height: 100%; color: var(--color-error); font-size: 14px; padding: 20px; text-align: center; }
</style>
