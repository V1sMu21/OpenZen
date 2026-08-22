<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import type { Artifact } from "../stores/sidepanel.svelte";

  let { artifact } = $props<{ artifact: Artifact }>();

  interface DiffLine {
    content: string;
    kind: "add" | "remove" | "context" | "header";
  }
  let lines = $state<DiffLine[]>([]);
  let loading = $state(true);
  let error = $state("");

  async function loadDiff() {
    loading = true; error = "";
    try {
      const raw = await invoke<string>("get_git_diff", { path: artifact.path });
      lines = parseDiff(raw);
    } catch (e) {
      error = String(e);
    }
    loading = false;
  }

  function parseDiff(raw: string): DiffLine[] {
    return raw.split("\n").map(line => {
      if (line.startsWith("+++") || line.startsWith("---") || line.startsWith("@@") || line.startsWith("diff")) {
        return { content: line, kind: "header" as const };
      }
      if (line.startsWith("+") && !line.startsWith("+++")) {
        return { content: line, kind: "add" as const };
      }
      if (line.startsWith("-") && !line.startsWith("---")) {
        return { content: line, kind: "remove" as const };
      }
      return { content: line, kind: "context" as const };
    }).filter(l => l.content.trim() !== "");
  }

  onMount(() => { loadDiff(); });
</script>

<div class="diff-view">
  {#if loading}
    <div class="diff-status">Computing diff...</div>
  {:else if error}
    <div class="diff-error">{error}</div>
  {:else}
    <div class="diff-scroll">
      {#each lines as l}
        <div class="diff-line diff-{l.kind}">
          <pre>{l.content}</pre>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .diff-view { height: 100%; display: flex; flex-direction: column; font-family: Menlo, Monaco, monospace; font-size: 12px; }
  .diff-scroll { flex: 1; overflow: auto; }
  .diff-line { padding: 0 12px; line-height: 1.4; }
  .diff-line pre { margin: 0; white-space: pre-wrap; word-break: break-all; }
  .diff-add { background: rgba(0, 200, 0, 0.08); color: #7ee07e; }
  .diff-remove { background: rgba(255, 60, 60, 0.08); color: #f08080; }
  .diff-header { color: var(--accent, #6c63ff); font-weight: 600; padding-top: 8px; }
  .diff-context { color: var(--text-muted, #888); }
  .diff-status, .diff-error { padding: 16px; text-align: center; color: var(--text-muted, #888); }
  .diff-error { color: var(--danger, #e53e3e); }
</style>
