<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import type { Artifact } from "../stores/sidepanel";

  let { artifact } = $props<{ artifact: Artifact }>();

  let htmlContent = $state<string>("");
  let loading = $state(true);
  let error = $state<string | null>(null);

  async function loadDocx() {
    try {
      const bytes: ArrayBuffer = await invoke("read_file_bytes", { path: artifact.path });
      const mammoth = await import("mammoth");
      const result = await mammoth.convertToHtml({ arrayBuffer: new Uint8Array(bytes).buffer });
      htmlContent = result.value;
      loading = false;
    } catch (e) {
      error = String(e);
      loading = false;
    }
  }

  onMount(() => { loadDocx(); });
</script>

<div class="docx-viewer">
  {#if error}
    <div class="docx-error">{error}</div>
  {:else if loading}
    <div class="docx-loading">Loading...</div>
  {:else}
    <div class="docx-content">{@html htmlContent}</div>
  {/if}
</div>

<style>
  .docx-viewer {
    height: 100%; overflow: auto; padding: 16px;
    color: var(--color-body); font-size: 14px; line-height: 1.6;
  }
  .docx-content :global(h1) { font-size: 1.5em; font-weight: 700; margin: 16px 0 8px; }
  .docx-content :global(h2) { font-size: 1.3em; font-weight: 600; margin: 14px 0 6px; }
  .docx-content :global(h3) { font-size: 1.15em; font-weight: 600; margin: 12px 0 4px; }
  .docx-content :global(p) { margin: 0 0 8px; }
  .docx-content :global(ul), .docx-content :global(ol) { margin: 0 0 8px; padding-left: 24px; }
  .docx-content :global(table) { border-collapse: collapse; margin: 8px 0; width: 100%; }
  .docx-content :global(th), .docx-content :global(td) {
    border: 1px solid var(--color-hairline); padding: 6px 10px; text-align: left;
  }
  .docx-content :global(img) { max-width: 100%; height: auto; }
  .docx-loading, .docx-error {
    display: flex; align-items: center; justify-content: center;
    height: 100%; color: var(--color-muted); font-size: 14px;
  }
  .docx-error { color: var(--color-error); }
</style>
