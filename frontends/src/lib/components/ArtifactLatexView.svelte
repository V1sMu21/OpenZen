<script lang="ts">
  import { onMount } from "svelte";
  import katex from "katex";
  import { invoke } from "@tauri-apps/api/core";
  import type { Artifact } from "../stores/sidepanel";

  let { artifact } = $props<{ artifact: Artifact }>();

  let html = $state("");
  let loading = $state(true);
  let error = $state<string | null>(null);

  function stripPreamble(content: string): string {
    const docStart = content.match(/\\begin\{document\}/);
    if (!docStart) return content;
    const startIdx = docStart.index! + docStart[0].length;
    let body = content.substring(startIdx);
    const docEnd = body.lastIndexOf("\\end{document}");
    if (docEnd >= 0) body = body.substring(0, docEnd);
    return body;
  }

  function renderLatexToHtml(content: string): string {
    const body = stripPreamble(content);
    let out = body;

    // ── Step 1: render display math $$...$$ and \[...\] ──
    out = out.replace(/\$\$([\s\S]+?)\$\$/g, (_m, math: string) => {
      try {
        return katex.renderToString(math.trim(), { displayMode: true, throwOnError: false, output: "html" });
      } catch { return `<div class="math-block-fallback">${escapeHtml(math.trim())}</div>`; }
    });
    out = out.replace(/\\\[([\s\S]+?)\\\]/g, (_m, math: string) => {
      try {
        return katex.renderToString(math.trim(), { displayMode: true, throwOnError: false, output: "html" });
      } catch { return `<div class="math-block-fallback">${escapeHtml(math.trim())}</div>`; }
    });

    // ── Step 2: render inline math $...$ and \(...\) ──
    out = out.replace(/\$([^$]+?)\$/g, (_m, math: string) => {
      try {
        return katex.renderToString(math.trim(), { displayMode: false, throwOnError: false, output: "html" });
      } catch { return escapeHtml(math); }
    });
    out = out.replace(/\\\(([\s\S]+?)\\\)/g, (_m, math: string) => {
      try {
        return katex.renderToString(math.trim(), { displayMode: false, throwOnError: false, output: "html" });
      } catch { return escapeHtml(math); }
    });

    // ── Step 3: section headings ──
    out = out.replace(/\\section\*?\{([^}]+)\}/g, '<h3 class="latex-section">$1</h3>');
    out = out.replace(/\\subsection\*?\{([^}]+)\}/g, '<h4 class="latex-subsection">$1</h4>');
    out = out.replace(/\\subsubsection\*?\{([^}]+)\}/g, '<h5 class="latex-subsubsection">$1</h5>');

    // ── Step 4: text formatting ──
    out = out.replace(/\\textbf\{([^}]+)\}/g, '<strong>$1</strong>');
    out = out.replace(/\\textit\{([^}]+)\}/g, '<em>$1</em>');
    out = out.replace(/\\texttt\{([^}]+)\}/g, '<code>$1</code>');
    out = out.replace(/\\emph\{([^}]+)\}/g, '<em>$1</em>');

    // ── Step 5: enumerate / itemize ──
    out = out.replace(
      /\\begin\{enumerate\}([\s\S]*?)\\end\{enumerate\}/g,
      (_m, items: string) => `<ol class="latex-enum">${items.replace(
        /\\item\s+([\s\S]*?)(?=\\item|$)/g,
        '<li>$1</li>'
      )}</ol>`
    );
    out = out.replace(
      /\\begin\{itemize\}([\s\S]*?)\\end\{itemize\}/g,
      (_m, items: string) => `<ul class="latex-itemize">${items.replace(
        /\\item\s+([\s\S]*?)(?=\\item|$)/g,
        '<li>$1</li>'
      )}</ul>`
    );

    // ── Step 6: figures / includegraphics → placeholder ──
    out = out.replace(/\\begin\{figure\}[\s\S]*?\\end\{figure\}/g, (_m) => {
      const src = _m.match(/\\includegraphics(?:\[[^\]]*\])?\{([^}]+)\}/)?.[1] ?? "";
      const caption = _m.match(/\\caption\{([^}]+)\}/)?.[1] ?? "";
      return `<figure class="latex-figure"><div class="latex-figure-placeholder">📐 ${escapeHtml(src)}</div>${caption ? `<figcaption>${escapeHtml(caption)}</figcaption>` : ""}</figure>`;
    });
    out = out.replace(/\\includegraphics(?:\[[^\]]*\])?\{([^}]+)\}/g, (_m, src: string) =>
      `<div class="latex-inline-figure-placeholder">📐 ${escapeHtml(src)}</div>`
    );

    // ── Step 7: \\ → <br>, blank lines → paragraphs ──
    out = out.replace(/\\\\/g, "<br>");
    out = out
      .split(/\n\n+/)
      .map((block) => block.trim())
      .filter(Boolean)
      .map((block) => {
        if (/^<(h[3-5]|ul|ol|div|figure|pre|table)/.test(block)) return block;
        return `<p class="latex-p">${block}</p>`;
      })
      .join("\n");

    // ── Step 8: strip remaining unsupported LaTeX commands ──
    out = out.replace(/\\[a-zA-Z]+(\{[^}]*\})*/g, " ");
    out = out.replace(/\{[^}]*\}/g, (m) => {
      if (/^<[a-zA-Z]/.test(m)) return m;
      return m.slice(1, -1);
    });

    return out;
  }

  function escapeHtml(s: string): string {
    return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  }

  onMount(async () => {
    try {
      const content = await invoke<string>("read_file_content", { path: artifact.path });
      html = renderLatexToHtml(content);
    } catch (e) {
      error = `Failed to load: ${e}`;
    }
    loading = false;
  });
</script>

{#if loading}
  <div class="latex-loading">Loading...</div>
{:else if error}
  <div class="latex-error">{error}</div>
{:else}
  <div class="latex-content">{@html html}</div>
{/if}

<style>
  .latex-loading,
  .latex-error {
    padding: 24px;
    text-align: center;
  }
  .latex-error { color: var(--danger, #e53e3e); }

  .latex-content {
    padding: 20px 24px;
    font-size: 14px;
    line-height: 1.8;
    color: var(--text-primary, #eee);
    overflow-wrap: break-word;
  }

  :global(.latex-content h3.latex-section) {
    font-size: 18px;
    font-weight: 700;
    margin: 28px 0 12px;
    padding-bottom: 6px;
    border-bottom: 1px solid var(--border-color, #333);
    color: var(--accent, #6c63ff);
  }
  :global(.latex-content h4.latex-subsection) {
    font-size: 15px;
    font-weight: 600;
    margin: 20px 0 8px;
    color: var(--text-primary, #eee);
  }
  :global(.latex-content h5.latex-subsubsection) {
    font-size: 13px;
    font-weight: 600;
    margin: 16px 0 6px;
    color: var(--text-secondary, #aaa);
  }
  :global(.latex-content p.latex-p) { margin: 8px 0; }
  :global(.latex-content .katex-display) { margin: 14px 0; overflow-x: auto; overflow-y: hidden; }
  :global(.latex-content .katex) { font-size: 1.1em; }

  :global(.latex-figure) {
    margin: 16px 0;
    text-align: center;
  }
  :global(.latex-figure-placeholder),
  :global(.latex-inline-figure-placeholder) {
    padding: 20px;
    background: var(--bg-surface, #1e1e3a);
    border: 1px dashed var(--border-color, #444);
    border-radius: 8px;
    font-family: monospace;
    font-size: 13px;
    color: var(--text-muted, #888);
  }
  :global(.latex-figure figcaption) {
    margin-top: 6px;
    font-size: 12px;
    color: var(--text-muted, #888);
  }

  :global(.latex-enum), :global(.latex-itemize) {
    margin: 8px 0 8px 24px;
  }
  :global(.latex-enum li), :global(.latex-itemize li) {
    margin: 4px 0;
  }

  :global(.math-block-fallback) {
    display: block;
    padding: 8px 12px;
    background: var(--bg-surface, #1e1e3a);
    border-radius: 4px;
    font-family: monospace;
    font-size: 13px;
    color: var(--text-secondary, #aaa);
    margin: 8px 0;
  }
</style>
