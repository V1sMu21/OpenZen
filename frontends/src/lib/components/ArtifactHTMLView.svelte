<script lang="ts">
  import type { Artifact } from "../stores/sidepanel.svelte";
  import { invoke } from "@tauri-apps/api/core";

  let { artifact } = $props<{ artifact: Artifact }>();

  let htmlContent = $state("");
  let error = $state<string | null>(null);
  let loading = $state(false);
  let refreshKey = $state(0);

  // The `ozfile://` custom scheme (registered in Rust) serves files with real
  // `/` path separators, so relative resources (css/js/img next to the html)
  // resolve correctly and the document has no CSP — games run like in a browser.
  // The built-in asset:// protocol cannot do this: convertFileSrc percent-encodes
  // the whole path, breaking relative URL resolution for multi-file HTML apps.
  // In plain-browser dev mode there is no ozfile scheme, so fall back to
  // reading content and rendering it via srcdoc.
  const isTauri = () =>
    typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

  let iframeSrc = $state("");

  function buildIframeSrc() {
    if (!isTauri()) return "";
    // Encode each path segment but keep the `/` separators literal.
    const encoded = artifact.path
      .split("/")
      .map(encodeURIComponent)
      .join("/");
    return `ozfile://localhost${encoded}?t=${Date.now()}`;
  }

  async function loadContent() {
    if (isTauri()) {
      iframeSrc = buildIframeSrc();
      loading = false;
      return;
    }
    loading = true;
    error = null;
    try {
      htmlContent = await invoke<string>("read_file_content", {
        path: artifact.path,
      });
    } catch (e) {
      error = String(e);
      htmlContent = "";
    } finally {
      loading = false;
    }
  }

  function refresh() {
    refreshKey++;
    loadContent();
  }

  $effect(() => {
    artifact.path;
    loadContent();
  });

  // Refresh when file changes on disk (via Tauri watcher event forwarded by store)
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

<div class="html-view">
  <div class="html-toolbar">
    <span class="file-name">{artifact.label}</span>
    <button class="refresh-btn" onclick={refresh} title="Refresh">🔄</button>
  </div>
  {#if error}
    <div class="html-error">
      <p>Failed to load: {error}</p>
    </div>
  {:else if loading}
    <div class="html-loading">
      <p>Loading...</p>
    </div>
  {:else if isTauri()}
    {#key refreshKey}
      <iframe
        class="html-iframe"
        src={iframeSrc}
        sandbox="allow-scripts allow-same-origin allow-modals"
        referrerpolicy="no-referrer"
        title={artifact.label}
      ></iframe>
    {/key}
  {:else}
    <iframe
      class="html-iframe"
      srcdoc={htmlContent}
      sandbox="allow-scripts allow-same-origin allow-modals"
      referrerpolicy="no-referrer"
      title={artifact.label}
    ></iframe>
  {/if}
</div>

<style>
  .html-view {
    display: flex;
    flex-direction: column;
    height: 100%;
  }
  .html-toolbar {
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
  .html-iframe {
    flex: 1;
    border: none;
    width: 100%;
    height: 100%;
  }
  .html-error,
  .html-loading {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--text-muted, #888);
    font-size: 14px;
  }
  .html-error {
    color: var(--danger, #e53e3e);
  }
</style>
