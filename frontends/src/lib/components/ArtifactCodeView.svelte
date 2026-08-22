<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import type { Artifact } from "../stores/sidepanel.svelte";

  let { artifact } = $props<{ artifact: Artifact }>();

  let code = $state("");
  let lang = $state("plaintext");
  let loading = $state(true);
  let errorMsg = $state("");

  function detectLang(path: string): string {
    const ext = path.split(".").pop()?.toLowerCase() ?? "";
    const map: Record<string, string> = {
      py: "python", rs: "rust", ts: "typescript", js: "javascript",
      json: "json", yaml: "yaml", yml: "yaml", toml: "toml",
      sql: "sql", sh: "bash", zsh: "bash", go: "go",
      svelte: "svelte", html: "html", htm: "html", css: "css", scss: "css",
      md: "markdown",
    };
    return map[ext] ?? "plaintext";
  }

  onMount(async () => {
    try {
      code = await invoke<string>("read_file_content", { path: artifact.path });
      lang = detectLang(artifact.path);
    } catch (e) {
      errorMsg = e instanceof Error ? e.message : String(e);
    }
    loading = false;
  });
</script>

<div class="code-view">
  {#if loading}
    <div class="code-loading">Loading…</div>
  {:else if errorMsg}
    <div class="code-error">{errorMsg}</div>
  {:else}
    <pre class="code-content">{code}</pre>
  {/if}
</div>

<style>
  .code-view { position: absolute; inset: 0; display: flex; flex-direction: column; }
  .code-content {
    flex: 1;
    margin: 0;
    padding: 12px 16px;
    overflow: auto;
    font-size: 13px; line-height: 1.5;
    font-family: Menlo, Monaco, 'Courier New', monospace;
    background: var(--bg-surface, #1a1a2e);
    color: var(--text-primary, #e0e0e0);
    white-space: pre; tab-size: 4;
  }
  .code-loading { padding: 24px; color: var(--text-muted, #888); text-align: center; }
  .code-error { padding: 24px; color: var(--danger, #e53e3e); font-family: monospace; font-size: 13px; white-space: pre-wrap; }
</style>
