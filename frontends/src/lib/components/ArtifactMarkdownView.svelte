<script lang="ts">
  import { onMount } from "svelte";
  import type { Artifact } from "../stores/sidepanel";
  import { invoke } from "@tauri-apps/api/core";
  import { renderMarkdown } from "../utils/markdown";

  let { artifact } = $props<{ artifact: Artifact }>();

  let raw = $state("");
  let loading = $state(true);
  let error = $state<string | null>(null);

  async function loadContent() {
    loading = true;
    error = null;
    try {
      raw = await invoke<string>("read_file_content", { path: artifact.path });
    } catch (e) {
      error = String(e);
      raw = "";
    } finally {
      loading = false;
    }
  }

  onMount(() => { loadContent(); });

  // Live-reload on file change
  $effect(() => {
    const handler = (e: Event) => {
      const detail = (e as CustomEvent)?.detail;
      if (detail?.path === artifact.path) {
        loadContent();
      }
    };
    window.addEventListener("sidepanel:file-changed", handler);
    return () => window.removeEventListener("sidepanel:file-changed", handler);
  });
</script>

<div class="md-view">
  <div class="md-toolbar">
    <span class="file-name">{artifact.label}</span>
    <button class="refresh-btn" onclick={loadContent} title="Refresh">🔄</button>
  </div>
  <div class="md-scroll">
    {#if error}
      <div class="md-error">Failed to load: {error}</div>
    {:else if loading}
      <div class="md-loading">Loading...</div>
    {:else}
      <div class="markdown-content">{@html renderMarkdown(raw, { highlight: false })}</div>
    {/if}
  </div>
</div>

<style>
  .md-view {
    height: 100%;
    display: flex;
    flex-direction: column;
  }
  .md-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 4px 8px;
    background: var(--bg-surface, #1a1a2e);
    border-bottom: 1px solid var(--border-color, #333);
    font-size: 12px;
    color: var(--text-muted, #888);
    flex-shrink: 0;
  }
  .refresh-btn {
    background: none;
    border: none;
    color: var(--text-muted, #888);
    cursor: pointer;
    font-size: 14px;
    padding: 2px 6px;
    border-radius: 4px;
  }
  .refresh-btn:hover {
    background: var(--bg-hover, #2a2a4e);
  }
  .md-scroll {
    flex: 1;
    overflow-y: auto;
    padding: 12px 16px;
  }
  .md-error {
    color: var(--danger, #e53e3e);
    text-align: center;
    padding: 20px;
    font-size: 14px;
  }
  .md-loading {
    color: var(--text-muted, #888);
    text-align: center;
    padding: 20px;
    font-size: 14px;
  }

  /* ── Markdown rendering styles ── */
  .markdown-content {
    line-height: 1.7;
    overflow-wrap: break-word;
    word-break: break-word;
    color: var(--text-primary, #e0e0e0);
    font-size: 14px;
  }
  .markdown-content :global(p) {
    margin: 0.5em 0;
  }
  .markdown-content :global(p:first-child) {
    margin-top: 0;
  }
  .markdown-content :global(p:last-child) {
    margin-bottom: 0;
  }
  .markdown-content :global(h1),
  .markdown-content :global(h2),
  .markdown-content :global(h3),
  .markdown-content :global(h4),
  .markdown-content :global(h5) {
    margin: 12px 0 6px 0;
    font-weight: 600;
  }
  .markdown-content :global(h1) { font-size: 18px; }
  .markdown-content :global(h2) { font-size: 16px; }
  .markdown-content :global(h3) { font-size: 15px; }
  .markdown-content :global(h4) { font-size: 14px; }
  .markdown-content :global(h5) { font-size: 13px; }
  .markdown-content :global(ul),
  .markdown-content :global(ol) {
    margin: 4px 0 8px 0;
    padding-left: 20px;
  }
  .markdown-content :global(li) {
    margin-bottom: 2px;
  }
  .markdown-content :global(pre) {
    margin: 8px 0;
    padding: 12px;
    border-radius: 8px;
    background: var(--bg-tertiary, #16213e);
    overflow-x: auto;
    white-space: pre;
    font-size: 13px;
    line-height: 1.5;
    border: 1px solid var(--border-color, #333);
  }
  .markdown-content :global(code) {
    font-family: var(--font-mono, Menlo, Monaco, 'Courier New', monospace);
    font-size: 0.9em;
    background: var(--bg-tertiary, #16213e);
    padding: 1px 5px;
    border-radius: 3px;
  }
  .markdown-content :global(pre code) {
    background: none;
    padding: 0;
    border-radius: 0;
  }
  .markdown-content :global(blockquote) {
    border-left: 3px solid var(--color-accent, #6c63ff);
    margin: 8px 0;
    padding: 4px 12px;
    color: var(--text-secondary, #aaa);
  }
  .markdown-content :global(table) {
    border-collapse: collapse;
    width: 100%;
    margin: 8px 0;
    font-size: 13px;
  }
  .markdown-content :global(th),
  .markdown-content :global(td) {
    border: 1px solid var(--border-color, #333);
    padding: 6px 10px;
    text-align: left;
  }
  .markdown-content :global(th) {
    background: var(--bg-tertiary, #16213e);
    font-weight: 600;
  }
  .markdown-content :global(img) {
    max-width: 100%;
    border-radius: 8px;
  }
  .markdown-content :global(hr) {
    border: none;
    border-top: 1px solid var(--border-color, #333);
    margin: 12px 0;
  }
  .markdown-content :global(strong) {
    font-weight: 600;
  }
  .markdown-content :global(.md-link) {
    color: var(--color-accent, #5b9bd5);
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .markdown-content :global(.md-link:hover) {
    text-decoration-thickness: 2px;
  }
  .markdown-content :global(.math-inline) {
    font-family: var(--font-mono, Menlo, Monaco, monospace);
    background: var(--color-surface-soft);
    padding: 1px 6px;
    border-radius: 4px;
    font-size: 0.95em;
  }
  .markdown-content :global(.math-block) {
    font-family: var(--font-mono, Menlo, Monaco, monospace);
    background: var(--color-surface-soft);
    padding: 12px 16px;
    border-radius: 8px;
    margin: 8px 0;
    overflow-x: auto;
    white-space: pre;
    font-size: 0.95em;
    text-align: center;
  }
</style>
