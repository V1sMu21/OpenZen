<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import type { Artifact } from "../stores/sidepanel.svelte";

  let { artifact } = $props<{ artifact: Artifact }>();

  let dataUrl = $state("");
  let error = $state<string | null>(null);
  let loading = $state(true);
  let zoom = $state(1);
  let naturalWidth = $state(0);
  let naturalHeight = $state(0);

  const MIME_BY_EXT: Record<string, string> = {
    png: "image/png",
    jpg: "image/jpeg",
    jpeg: "image/jpeg",
    gif: "image/gif",
    svg: "image/svg+xml",
    webp: "image/webp",
    bmp: "image/bmp",
    ico: "image/x-icon",
    avif: "image/avif",
    tiff: "image/tiff",
    tif: "image/tiff",
  };
  const SUPPORTED = new Set(Object.keys(MIME_BY_EXT));

  function mimeFor(path: string): string {
    const ext = (path.split(".").pop() ?? "").toLowerCase();
    return MIME_BY_EXT[ext] ?? "image/png";
  }

  // `String.fromCharCode(...bytes)` blows the stack the moment the image is
  // ~100 KB; chunked conversion keeps it linear in time and bounded in stack
  // depth so multi-MB images render without crashing the WebView.
  function bytesToBase64(bytes: Uint8Array): string {
    const CHUNK = 32 * 1024;
    let bin = "";
    for (let i = 0; i < bytes.length; i += CHUNK) {
      const slice = bytes.subarray(i, Math.min(i + CHUNK, bytes.length));
      bin += String.fromCharCode.apply(null, slice as unknown as number[]);
    }
    return btoa(bin);
  }

  onMount(async () => {
    const ext = (artifact.path.split(".").pop() ?? "").toLowerCase();
    if (!SUPPORTED.has(ext)) {
      error = `Unsupported image format — .${ext}. Supported: ${Array.from(SUPPORTED).join(", ")}.`;
      loading = false;
      return;
    }

    try {
      const bytes = await invoke<ArrayBuffer>("read_file_bytes", { path: artifact.path });
      const u8 = new Uint8Array(bytes);
      const mime = mimeFor(artifact.path);
      const b64 = bytesToBase64(u8);
      dataUrl = `data:${mime};base64,${b64}`;
    } catch (e) {
      error = `Failed to load image: ${e instanceof Error ? e.message : String(e)}`;
    }
    loading = false;
  });

  function onLoad(e: Event) {
    const img = e.target as HTMLImageElement;
    naturalWidth = img.naturalWidth;
    naturalHeight = img.naturalHeight;
  }
</script>

{#if loading}
  <div class="image-loading">Loading…</div>
{:else if error}
  <div class="image-error">{error}</div>
{:else if dataUrl}
  <div class="image-container">
    <img
      src={dataUrl}
      alt={artifact.label}
      class="image-viewer"
      onload={onLoad}
    />
  </div>
  {#if naturalWidth > 0}
    <div class="image-meta">
      <span>{artifact.label}</span>
      <span>{naturalWidth}×{naturalHeight}</span>
    </div>
  {/if}
{/if}

<style>
  .image-loading, .image-error {
    padding: 24px;
    text-align: center;
  }
  .image-error { color: var(--danger, #e53e3e); }
  .image-container {
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 200px;
    padding: 16px;
    background: var(--bg-main, #111);
  }
  .image-viewer {
    max-width: 100%;
    max-height: 80vh;
    object-fit: contain;
    border-radius: 4px;
  }
  .image-meta {
    display: flex;
    justify-content: space-between;
    padding: 6px 12px;
    font-size: 11px;
    color: var(--text-muted, #888);
    border-top: 1px solid var(--border-color, #333);
  }
</style>
