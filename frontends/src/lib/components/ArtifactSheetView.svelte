<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import type { Artifact } from "../stores/sidepanel.svelte";

  let { artifact } = $props<{ artifact: Artifact }>();

  let data = $state<string[][]>([]);
  let loading = $state(true);
  let error = $state("");

  async function loadSheet() {
    loading = true; error = "";
    try {
      const ext = artifact.path.split(".").pop()?.toLowerCase() ?? "";
      if (ext === "csv" || ext === "tsv") {
        const raw = await invoke<string>("read_file_content", { path: artifact.path });
        const sep = ext === "tsv" ? "\t" : ",";
        data = raw.split("\n").filter(r => r.trim()).map(r =>
          r.split(sep).map(c => c.trim().replace(/^"|"$/g, ""))
        );
      } else {
        const result = await invoke<string[][]>("parse_excel", { path: artifact.path });
        data = result;
      }
    } catch (e) {
      error = String(e);
    }
    loading = false;
  }

  onMount(() => { loadSheet(); });
</script>

<div class="sheet-view">
  {#if loading}
    <div class="sheet-loading">Loading spreadsheet...</div>
  {:else if error}
    <div class="sheet-error">{error}</div>
  {:else}
    <div class="sheet-table-wrap">
      <table>
        <tbody>
          {#each data as row}
            <tr>
              {#each row as cell}
                <td>{cell}</td>
              {/each}
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>

<style>
  .sheet-view { height: 100%; display: flex; flex-direction: column; }
  .sheet-table-wrap { flex: 1; overflow: auto; }
  table { border-collapse: collapse; font-size: 12px; }
  td {
    border: 1px solid var(--border-color, #333); padding: 4px 8px;
    white-space: nowrap; max-width: 300px; overflow: hidden;
    text-overflow: ellipsis; color: var(--text-primary, #e0e0e0);
  }
  tr:nth-child(even) { background: rgba(255,255,255,0.03); }
  tr:hover { background: var(--bg-hover, #2a2a4e); }
  .sheet-loading, .sheet-error {
    padding: 20px; color: var(--text-muted, #888); text-align: center;
  }
  .sheet-error { color: var(--danger, #e53e3e); }
</style>
