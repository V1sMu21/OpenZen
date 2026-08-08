<script lang="ts">
  import type { Artifact } from "../stores/sidepanel";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import { t } from "../i18n";

  let { artifact } = $props<{ artifact: Artifact }>();

  let fileSize = $state<string>("");
  let modTime = $state<string>("");
  let opening = $state(false);

  async function loadInfo() {
    try {
      const info: { size: number; modified: string } = await invoke("get_file_info", {
        path: artifact.path,
      });
      fileSize = info.size > 1024 * 1024
        ? `${(info.size / (1024 * 1024)).toFixed(1)} MB`
        : `${(info.size / 1024).toFixed(1)} KB`;
      modTime = new Date(info.modified).toLocaleString();
    } catch {
      fileSize = "—";
      modTime = "—";
    }
  }

  async function openExternally() {
    opening = true;
    try {
      await invoke("open_external_file", { path: artifact.path });
    } catch (e) {
      console.warn("open_external_file failed:", e);
    } finally {
      opening = false;
    }
  }

  $effect(() => { loadInfo(); });
</script>

<div class="office-viewer">
  <div class="office-icon">{artifact.type === "spreadsheet" ? "📊" : "📄"}</div>
  <p class="office-name">{artifact.label}</p>
  <p class="office-path">{artifact.path}</p>
  <div class="office-meta">
    <span>{$t("sidepanel.size")}: {fileSize}</span>
    <span>{$t("sidepanel.modified")}: {modTime}</span>
  </div>
  <p class="office-hint">
    {$t("sidepanel.officePreviewHint")}
  </p>
  <button class="office-open-btn" onclick={openExternally} disabled={opening}>
    {opening ? $t("sidepanel.opening") : $t("sidepanel.openExternal")}
  </button>
</div>

<style>
  .office-viewer {
    display: flex; flex-direction: column; align-items: center;
    justify-content: center; height: 100%; padding: 24px;
    text-align: center; color: var(--color-body);
  }
  .office-icon { font-size: 48px; margin-bottom: 12px; opacity: 0.5; }
  .office-name { font-size: 15px; font-weight: 600; margin: 0 0 4px; }
  .office-path { font-size: 11px; color: var(--color-muted); margin: 0 0 12px;
    word-break: break-all; max-width: 100%; }
  .office-meta { display: flex; gap: 16px; font-size: 11px;
    color: var(--color-dim); margin-bottom: 12px; }
  .office-hint { font-size: 12px; color: var(--color-muted);
    margin: 0 0 16px; max-width: 300px; line-height: 1.5; }
  .office-open-btn {
    padding: 8px 20px; border: 1px solid var(--color-primary);
    border-radius: 6px; background: var(--color-surface-elevated);
    color: var(--color-primary); cursor: pointer; font-size: 13px;
    font-weight: 500; transition: background 0.15s;
  }
  .office-open-btn:hover { background: var(--color-primary-muted); }
  .office-open-btn:disabled { opacity: 0.5; cursor: default; }
</style>
