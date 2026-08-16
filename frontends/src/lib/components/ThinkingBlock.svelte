<script lang="ts">
  import { onMount } from "svelte";
  import { t, locale, tSync } from "../i18n";
  let lang = $state("zh");
  $effect(() => { lang = $locale; });

  let {
    thinking = "",
    duration = 0,
    durationMs = undefined as number | undefined,
    streaming = false,
    showTimer = false,
    runningTool = "",
    showPausedWarning = true,
  } = $props();

  let collapsed = $state(true);

  function toggle() {
    collapsed = !collapsed;
  }

  function formatDuration(ms: number): string {
    if (ms < 1000) return `${ms}${tSync(lang, "message.duration.ms")}`;
    return tSync(lang, "message.duration.formatSec").replace("{s}", (ms / 1000).toFixed(1));
  }

  function wordCount(text: string): number {
    const trimmed = text.trim();
    if (!trimmed) return 0;
    // Count ASCII word runs (e.g. "don't", "Test123") AND each CJK /
    // Hangul / kana character as one unit. This handles both
    // English (e.g. Qwen's "Theuserwantsme") and Chinese / Japanese /
    // Korean (e.g. "用户让我重新写一个HTML文件") reasoning traces
    // correctly. The CJK ranges cover:
    //   U+3040–U+309F  Hiragana
    //   U+30A0–U+30FF  Katakana
    //   U+4E00–U+9FFF  CJK Unified Ideographs (the bulk of Chinese)
    //   U+3400–U+4DBF  CJK Extension A
    //   U+AC00–U+D7AF  Hangul Syllables
    const matches = trimmed.match(
      /[A-Za-z0-9]+(?:[''][A-Za-z0-9]+)?|[　-〿一-鿿가-힯゠-ヿ]/g
    );
    return matches ? matches.length : 0;
  }

  /**
   * Some models (notably Qwen reasoning traces) output their chain of
   * thought as a single run with no spaces between words — the result
   * is unreadable. We do a *display-only* pass that:
   *   1. inserts a space at every lowercase→uppercase boundary
   *      (camelCase splitting, e.g. "ReadingTheFile" → "Reading The File"),
   *   2. inserts a space at letter↔digit boundaries
   *      ("Test123Done" → "Test 123 Done"),
   *   3. inserts a space after a punctuation run that the model
   *      forgot to space after ("Hello,World" → "Hello, World").
   * The original (compressed) text is still used for word counting
   * and duration math — only the visible copy is decompressed.
   */
  function decompressText(raw: string): string {
    return raw
      // camelCase / PascalCase split: lower→upper or upper-upper→lower
      .replace(/([a-z])([A-Z])/g, "$1 $2")
      .replace(/([A-Z]+)([A-Z][a-z])/g, "$1 $2")
      // letter ↔ digit boundary
      .replace(/([A-Za-z])([0-9])/g, "$1 $2")
      .replace(/([0-9])([A-Za-z])/g, "$1 $2")
      // punctuation that the model forgot to space after
      .replace(/([.,;:?!])([A-Za-z0-9])/g, "$1 $2");
  }

  // Strip stray HTML/XML tags and Cursor paste markers from thinking content,
  // then decompress whitespace-free model output for readable display.
  // The full-text pipeline (8 regex passes) used to be a $derived recomputed
  // on EVERY token during streaming — O(n²) regex work per block. It is now
  // trailing-edge-throttled state: at most one recompute per ~50ms batch of
  // tokens, with the first chunk rendered immediately.
  const CLEAN_THROTTLE_MS = 50;
  let cleanDisplay = $state("");
  let wordN = $state(0);
  let throttleTimer: ReturnType<typeof setTimeout> | null = null;
  let lastRawThinking = "";

  function applyClean(raw: string) {
    const cleaned = decompressText(
      raw
        .replace(/\[Pasted ~[^\]]+\]/g, "")
        .replace(/<[^>]*>/g, "")
        .replace(/^\s+/, "")
        .trim()
    );
    cleanDisplay = cleaned;
    wordN = wordCount(cleaned);
  }

  $effect(() => {
    const raw = thinking;
    if (raw === lastRawThinking) return;
    const isFirst = lastRawThinking === "";
    lastRawThinking = raw;
    if (throttleTimer !== null) return; // batched — trailing edge uses latest raw
    throttleTimer = setTimeout(() => {
      throttleTimer = null;
      applyClean(lastRawThinking);
    }, isFirst ? 0 : CLEAN_THROTTLE_MS);
  });

  onMount(() => {
    return () => {
      // Cancel a pending throttle on unmount — nothing may write state
      // after teardown.
      if (throttleTimer !== null) {
        clearTimeout(throttleTimer);
        throttleTimer = null;
      }
    };
  });

  let wordsLabel = $derived.by(() => {
    if (wordN === 0) return "";
    if (wordN === 1) return tSync(lang, "thinking.word");
    return tSync(lang, "thinking.words").replace("{n}", String(wordN));
  });

  // Header duration — accumulated active thinking time.
  // Instead of measuring wall-clock from the first token to the last
  // (which includes gaps when the model is executing tools), we track
  // the *word count growth* and only count time when thinking content
  // is actively expanding.
  //
  // Concept: when wordCount changes → thinking is happening.
  //          when wordCount is stable for >3s → thinking is paused.
  const PAUSE_THRESHOLD_MS = 1200;
  let liveTickMs = $state(0);
  let lastGrowthMs = 0;       // wall-clock ms of the last word-count change
  let accumulatedMs = 0;      // total active thinking ms so far
  let isPaused = $state(false);
  let liveInterval: ReturnType<typeof setInterval> | null = null;
  let capturedDurationMs = $state(0);
  let prevWordCount = 0;

  function startLiveTimer() {
    if (liveInterval !== null) return;
    const now = Date.now();
    lastGrowthMs = now;
    accumulatedMs = 0;
    liveTickMs = 0;
    isPaused = false;
    capturedDurationMs = 0;
    prevWordCount = 0;
    liveInterval = setInterval(() => {
      const t = Date.now();
      const sinceGrowth = t - lastGrowthMs;
      if (sinceGrowth > PAUSE_THRESHOLD_MS) {
        liveTickMs = accumulatedMs;
        isPaused = true;
      } else {
        liveTickMs = accumulatedMs + sinceGrowth;
        isPaused = false;
      }
    }, 250);
  }

  function stopLiveTimer() {
    if (liveInterval !== null) {
      clearInterval(liveInterval);
      liveInterval = null;
    }
    capturedDurationMs = accumulatedMs;
  }

  // Track word-count growth to accumulate active thinking time.
  // Depends on throttled `wordN` instead of recomputing wordCount() over the
  // full text on every token.
  $effect(() => {
    if (!streaming) return;
    const wc = wordN;
    const now = Date.now();
    if (wc !== prevWordCount) {
      const since = now - lastGrowthMs;
      accumulatedMs += Math.min(since, PAUSE_THRESHOLD_MS);
      lastGrowthMs = now;
      prevWordCount = wc;
      isPaused = false;
    }
  });

  $effect(() => {
    if (streaming) {
      startLiveTimer();
    } else {
      stopLiveTimer();
      isPaused = false;
    }
    return stopLiveTimer;
  });

  // When streaming ends, prefer durationMs from the reasoning_end timing
  // (computed by protocol-processor.ts from actual reasoning_start→end).
  // This is the exact server-declared thinking duration per block, not
  // wall-clock including tool gaps or word-count heuristics.
  let headerDuration = $derived(
    streaming ? 0 : (
      (durationMs != null && durationMs > 200 ? durationMs : 0)
      || capturedDurationMs
      || (duration ?? 0)
    )
  );
  let headerDurationLabel = $derived(
    headerDuration > 0 ? formatDuration(headerDuration) : ""
  );
</script>

{#if thinking}
  <div class="thinking-block">
    <button class="thinking-header" onclick={toggle} aria-expanded={!collapsed} aria-label={$t("thinking.label")}>
      <span class="tglyph">⚘</span>
      <span class="thinking-label">{$t("thinking.label")}</span>
      {#if headerDurationLabel && showTimer}
        <span class="thinking-duration">{headerDurationLabel}</span>
      {/if}
      {#if runningTool}
        <span class="thinking-paused thinking-tool-active" title={runningTool}>{runningTool}</span>
      {:else if isPaused && showPausedWarning}
        <span class="thinking-paused" title={$t("thinking.pausedTitle")}>{$t("thinking.paused")}</span>
      {/if}
      <span class="thinking-words">{wordsLabel}</span>
    </button>
    <div class="thinking-body" class:open={!collapsed}>
      <div class="thinking-inner">
        <div class="thinking-content">{cleanDisplay}</div>
        {#if streaming}
          <span class="wave" aria-hidden="true"><span>∿</span><span>∿</span><span>∿</span></span>
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  .thinking-block {
    /* 楷体手迹: 无卡片, 静思行 */
    margin: 2px 0 2px;
    border-bottom: 1px solid var(--color-hairline);
    border-radius: 0;
    padding-left: 0;
  }

  .thinking-header {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 4px 10px;
    background: none;
    border: none;
    color: var(--color-body);
    cursor: pointer;
    font-family: var(--font-kai);
    font-size: 13px;
    letter-spacing: 0.06em;
    text-align: left;
    transition: background 0.35s var(--ease-soak, ease);
    border-radius: 3px;
  }

  .thinking-header:hover {
    background: var(--color-primary-muted, rgba(147,195,214,0.07));
  }

  .tglyph {
    flex-shrink: 0;
    color: var(--color-primary);
    font-size: 11px;
    opacity: 0.8;
  }

  .thinking-label {
    font-weight: 500;
    color: var(--color-primary);
    font-size: 13px;
    letter-spacing: 0.15em;
  }

  .thinking-duration {
    color: var(--color-dim);
    font-family: var(--font-mono);
    font-size: 10px;
    font-weight: 500;
    font-variant-numeric: tabular-nums;
  }

  .thinking-words {
    margin-left: auto;
    color: var(--color-dim);
    font-size: 10.5px;
    opacity: 0.7;
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
  }

  .thinking-paused {
    color: #f59e0b;
    font-family: var(--font-kai);
    font-size: 11px;
    background: rgba(245, 158, 11, 0.10);
    padding: 1px 6px;
    border-radius: 2px;
    font-weight: 500;
    animation: pausedBlink 1.6s ease-in-out infinite;
  }

  .thinking-tool-active {
    color: #10b981;
    background: rgba(16, 185, 129, 0.10);
    animation: none;
    text-transform: none;
    letter-spacing: normal;
  }

  @keyframes pausedBlink {
    0%, 100% { opacity: 0.6; }
    50%      { opacity: 1; }
  }

  /* 展开: 入釉浸润 (grid-rows 0fr→1fr) */
  .thinking-body {
    display: grid;
    grid-template-rows: 0fr;
    transition: grid-template-rows 0.5s var(--ease-soak, ease);
  }
  .thinking-body.open {
    grid-template-rows: 1fr;
  }
  .thinking-inner {
    overflow: hidden;
  }

  .thinking-content {
    font-family: var(--font-kai);
    font-size: 13.5px;
    line-height: 2;
    white-space: pre-wrap;
    word-break: break-word;
    color: var(--color-body);
    padding: 6px 8px 10px;
  }

  .wave {
    display: inline-flex;
    gap: 2px;
    margin-left: 2px;
    color: var(--color-primary);
    font-size: 13px;
    line-height: 1;
    vertical-align: baseline;
    user-select: none;
  }
  .wave span {
    display: inline-block;
    animation: waveBounce 1.2s ease-in-out infinite;
  }
  .wave span:nth-child(1) { animation-delay: 0s; }
  .wave span:nth-child(2) { animation-delay: 0.15s; }
  .wave span:nth-child(3) { animation-delay: 0.3s; }

  @keyframes waveBounce {
    0%, 100% { transform: translateY(0); opacity: 0.45; }
    50%      { transform: translateY(-3px); opacity: 1; }
  }
</style>
