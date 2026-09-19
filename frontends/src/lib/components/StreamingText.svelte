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
  // Trailing '\n's that are consumed (renderedLen advanced past them) but
  // deliberately NOT painted as <br>. A text part almost always ends with
  // "\n\n" right before the next part (tool call) arrives — painting them
  // showed one-to-two blank lines between the reply and the first card
  // below it, a gap that vanished only at finalize (renderMarkdown trims).
  // Held newlines are restored on the next append: once content follows
  // them they are interior paragraph breaks again.
  let tailHold = 0;
  let rafId: number | undefined;
  // Full re-render is triggered when a large jump happens (e.g.
  // historical session restore) or when the pending slice grows
  // beyond the inline-render window (block-level syntax arrived).
  const INLINE_WINDOW = 400;

  function commitAppend() {
    rafId = undefined;
    if (!containerEl || pendingText.length === 0) return;
    // Restore held newlines: with new content following they are interior.
    let slice = "\n".repeat(tailHold) + pendingText;
    tailHold = 0;
    // Hold back the new trailing newlines (see tailHold above).
    const tail = slice.match(/\n+$/);
    if (tail) {
      tailHold = tail[0].length;
      slice = slice.slice(0, slice.length - tailHold);
    }
    // Block-level syntax in a large slice needs a structural render;
    // plain-text bulk (fast local prefill) appends in segments so the
    // work per frame stays O(delta) instead of re-rendering the whole
    // message (which made long prefills quadratic).
    const BLOCK_TRIGGER =
      /(?:^|\n)(?:#{1,6} |```|> |\| |-{3,}|={3,}|\*\*\*|\d+\. )/;
    if (slice.length > INLINE_WINDOW && BLOCK_TRIGGER.test(slice)) {
      // Full repaint matching the final `renderMarkdown` pass: it trims
      // both ends, so strip leading whitespace and drop the trailing
      // run entirely (nothing is held after a full repaint).
      containerEl.innerHTML = renderStreamingFragment(
        text.replace(/^\s+/, "").replace(/\s+$/, ""),
      );
      tailHold = 0;
    } else if (slice.length > INLINE_WINDOW) {
      for (let off = 0; off < slice.length; off += INLINE_WINDOW) {
        containerEl.insertAdjacentHTML(
          "beforeend",
          renderStreamingFragment(slice.slice(off, off + INLINE_WINDOW)),
        );
      }
    } else {
      // Strip leading whitespace on the FIRST append only: models
      // typically start the reply with "\n" after reasoning, which
      // would otherwise render as a blank <br> row — the "text far
      // from the thinking card" gap that disappears once the part is
      // finalized (renderMarkdown trims). Subsequent slices keep
      // their newlines so paragraph breaks still render.
      if (containerEl.childNodes.length === 0) {
        slice = slice.replace(/^\s+/, "");
        if (slice.length === 0) {
          // First frame was only whitespace — drop it permanently (it is
          // leading whitespace of the eventual text, not a trailing run).
          tailHold = 0;
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
      tailHold = 0;
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
