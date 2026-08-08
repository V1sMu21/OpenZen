<script lang="ts">
  import { renderStreamingFragment } from "../utils/markdown";

  let { text = $bindable("") } = $props();

  // ── Incremental streaming render ──
  // During live streaming we must NOT re-render the whole markdown
  // on every delta: renderMarkdown() is O(n) in the full text and
  // innerHTML replacement rebuilds the entire DOM subtree, so a long
  // reply would degrade to O(n²) work per token. Instead we commit
  // only the newly-arrived slice of text (lightweight inline render)
  // and let ChatMessage's `parts` loop do the precise full markdown
  // render once the part reaches `done` state.
  let containerEl: HTMLDivElement | undefined = $state();
  let renderedLen = 0;
  let pendingText = "";
  let rafId: number | undefined;
  // Full re-render is triggered when a large jump happens (e.g.
  // historical session restore) or when the pending slice grows
  // beyond the inline-render window (block-level syntax arrived).
  const INLINE_WINDOW = 400;

  function commitAppend() {
    rafId = undefined;
    if (!containerEl || pendingText.length === 0) return;
    if (pendingText.length > INLINE_WINDOW) {
      // Slice is large (block-level content): fall back to a full
      // render of the entire text so markdown structure stays intact.
      // Strip leading whitespace to match the final `renderMarkdown`
      // pass (it trims the text), so the live view has the same
      // top spacing as the finalized one.
      containerEl.innerHTML = renderStreamingFragment(text.replace(/^\s+/, ""));
    } else {
      // Strip leading whitespace on the FIRST append only: models
      // typically start the reply with "\n" after reasoning, which
      // would otherwise render as a blank <br> row — the "text far
      // from the thinking card" gap that disappears once the part is
      // finalized (renderMarkdown trims). Subsequent slices keep
      // their newlines so paragraph breaks still render.
      let slice = pendingText;
      if (containerEl.childNodes.length === 0) {
        slice = slice.replace(/^\s+/, "");
        if (slice.length === 0) {
          // First frame was only whitespace — advance the pointer so
          // the skipped newlines are not re-rendered on the next delta.
          renderedLen = text.length;
          pendingText = "";
          return;
        }
      }
      containerEl.insertAdjacentHTML("beforeend", renderStreamingFragment(slice));
    }
    renderedLen = text.length;
    pendingText = "";
  }

  $effect(() => {
    // text changed
    if (text.length > renderedLen + INLINE_WINDOW) {
      // Large jump (restore / first chunk) — reset and re-render all.
      if (containerEl) containerEl.innerHTML = "";
      renderedLen = 0;
      pendingText = "";
    }
    if (text.length > renderedLen) {
      pendingText = text.slice(renderedLen);
      if (rafId === undefined) {
        rafId = requestAnimationFrame(commitAppend);
      }
    }
  });
</script>

<div class="streaming-text">
  {#if text}
    <div bind:this={containerEl} class="markdown-content streaming-content"></div>
  {/if}
</div>

<style>
  .streaming-text {
    display: block;
  }

  /* The streaming content is inline (not block) so it flows directly
     after the last character instead of dropping to a line of its own.
     A block container + sibling indicator made the cursor sit on an
     empty line below the text, and when a fast prefill emitted a
     multi-paragraph first chunk, that line visibly jumped down several
     lines at once — looking like reserved space for the next card.
     Inline keeps the text end glued to the content (no cursor
     down-move). */
  .streaming-content {
    display: inline;
  }
</style>
