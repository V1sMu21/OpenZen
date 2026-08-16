<script lang="ts">
  import type { Component } from "svelte";
  import { t } from "../i18n";
  import type { Artifact } from "../stores/sidepanel.svelte";

  let {
    renderer,
    artifact,
  }: {
    renderer: string;
    artifact: Artifact;
  } = $props();

  type ArtifactRendererComponent = Component<{ artifact: Artifact }>;

  // Each loader is a dynamic import. Vite splits every renderer into its
  // own chunk, so the heavy dependency graphs (pdfjs-dist, @xterm/xterm,
  // katex via markdown/code views, mammoth) no longer sit in the main
  // bundle. A loader is only evaluated when its renderer is first shown.
  function loaderFor(kind: string): (() => Promise<{ default: ArtifactRendererComponent }>) | null {
    switch (kind) {
      case "html": return () => import("./ArtifactHTMLView.svelte");
      case "terminal": return () => import("./ArtifactTerminal.svelte");
      case "markdown": return () => import("./ArtifactMarkdownView.svelte");
      case "pdf": return () => import("./ArtifactPDFView.svelte");
      case "spreadsheet": return () => import("./ArtifactSheetView.svelte");
      case "code": return () => import("./ArtifactCodeView.svelte");
      case "diff": return () => import("./ArtifactDiffView.svelte");
      case "latex": return () => import("./ArtifactLatexView.svelte");
      case "image": return () => import("./ArtifactImageView.svelte");
      case "office":
        return artifact.path.toLowerCase().endsWith(".docx")
          ? () => import("./ArtifactDocxView.svelte")
          : () => import("./ArtifactOfficeView.svelte");
      default: return null;
    }
  }

  let Renderer: ArtifactRendererComponent | null = $state(null);
  let loadError = $state<string | null>(null);

  $effect(() => {
    let cancelled = false;
    Renderer = null;
    loadError = null;
    const loader = loaderFor(renderer);
    if (!loader) return;
    loader()
      .then((mod) => {
        if (!cancelled) Renderer = mod.default;
      })
      .catch((err) => {
        if (!cancelled) loadError = err instanceof Error ? err.message : String(err);
      });
    return () => {
      cancelled = true;
    };
  });
</script>

{#if Renderer}
  <svelte:component this={Renderer} {artifact} />
{:else if loadError}
  <div class="sidepanel-placeholder" role="alert">
    <p>📄 {artifact.label}</p>
    <p class="hint">{$t("sidepanel.loadError", loadError)}</p>
  </div>
{:else}
  <div class="sidepanel-placeholder">
    <p>📄 {artifact.label}</p>
    <p class="hint">{$t("sidepanel.loading", "Loading…")}</p>
  </div>
{/if}

<style>
  .sidepanel-placeholder {
    display: flex;
    flex-direction: column;
    gap: 8px;
    align-items: flex-start;
    justify-content: center;
    height: 100%;
    padding: 24px;
    color: var(--text-secondary);
  }
  .sidepanel-placeholder .hint {
    font-size: 12px;
    color: var(--text-tertiary);
    white-space: pre-wrap;
  }
</style>
