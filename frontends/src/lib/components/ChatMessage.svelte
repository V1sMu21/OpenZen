  <script lang="ts">
  import { onMount, untrack } from "svelte";
  import type { ToolCallInfo } from "../stores/types";
  import type { UIMessagePart, ToolInvocationPart } from "../stores/parts";
  import { convertStreamEventsToParts } from "../stores/parts";
  import { chat } from "../stores/chat";
  import { renderMarkdown } from "../utils/markdown";
  import { tickerState, useTicker } from "../utils/ticker.svelte";
  import StreamingText from "./StreamingText.svelte";
  import ToolCallCard from "./ToolCallCard.svelte";
  import ThinkingBlock from "./ThinkingBlock.svelte";
  import EditCard from "./EditCard.svelte";
import SkillMcpCard from "./SkillMcpCard.svelte";
import { t, locale, tSync } from "../i18n";
  let lang = $state("zh");
  $effect(() => { lang = $locale; });


  function isSkillOrMcpInvocation(name: string): boolean {
    if (name.startsWith("mcp__")) return true;
    if (name === "skill_mcp_search" || name === "skill_mcp_list") return true;
    return false;
  }

  // Stable empty array for non-live messages: historical bubbles never
  // receive a new array reference when streaming parts change.
  const NO_STREAMING_PARTS: UIMessagePart[] = [];

  let {
    message,
    showTimer = false,
    workingDir = "",
    // Computed in App.svelte, which is the only component that needs the
    // global chat-store state. Previously every ChatMessage subscribed to
    // the chat store; N subscriptions made every streaming delta O(N).
    // The `isLive` predicate is EXACTLY the four-condition predicate from
    // docs/correct-rendering-spec.md §3.2 — it just moved up one level.
    isLive = false,
    streamingParts = NO_STREAMING_PARTS,
    canRegenerate = false,
  } = $props();

  // ── Derived reactive values ──
  //
  //  See docs/correct-rendering-spec.md §二 for the correct rendering
  //  behavior of every derived signal in all three time-states.
  //
  // `isLive` is a prop now. The predicate lives in App.svelte and is:
  //   storeIsProcessing
  //   && last.role === "assistant"
  //   && storeMessages.length > 0
  //   && last.id === message.id
  // which guarantees only the latest assistant turn is live.

  /** "is this turn done?" — true when this assistant message has been
   *  finalized.  The authoritative finalize markers are `duration > 0`
   *  and a non-empty `exitReason` (both set exclusively by
   *  `finalizeAssistantMessage`).
   *
   *  We deliberately do NOT include `message.streaming !== true`,
   *  because `message.streaming` is `false` for every message loaded
   *  from disk — that OR branch would make `isRunning` permanently
   *  false for historical sessions (the "Done drowning out Running" bug). */
  let hasFinished = $derived(
    message.role === "assistant"
    && ((message.duration != null && message.duration > 0)
        || (message.exitReason != null && message.exitReason.length > 0))
  );

  /** The parts to render: streaming parts for live messages, saved
   *  parts otherwise.  The part currently rendered by the
   *  streaming-zone (see `zoneTextPart`) is excluded here to avoid
   *  double-rendering — StreamingText handles it exclusively. */
  let parts = $derived.by<UIMessagePart[]>(() => {
    if (isLive) {
      const zoneId = zoneTextPart?.id;
      return streamingParts.filter((p) => {
        if (p.type !== 'text') return true;
        if (p.state === 'streaming') return false;
        if (zoneId != null && p.id === zoneId) return false;
        return true;
      });
    }
    if (message.parts && message.parts.length > 0) return message.parts;
    if (message.streamEvents && message.streamEvents.length > 0) return convertStreamEventsToParts(message.streamEvents);
    return [];
  });

  /** The text part currently rendered by the streaming-zone, with the
   *  glued three-dot indicator. Priority:
   *   1. the last `streaming` text part (live token stream);
   *   2. the last text part when it is the FINAL part of the turn and
   *      no tool is running — i.e. the model finished the text and is
   *      prefilling the next action (e.g. an edit tool call).
   *  Keeping the just-finished text in the zone keeps the dots glued
   *  to the text end instead of dropping them into a separate line
   *  below the card — the "dots moved down before the tool card
   *  appears" bug. The part only leaves the zone when the next
   *  content (tool card / next text) actually arrives, at which point
   *  the loop renders it with the precise markdown. */
  let zoneTextPart = $derived.by<{ id: string; text: string } | null>(() => {
    if (!isLive) return null;
    const arr = streamingParts;
    for (let i = arr.length - 1; i >= 0; i--) {
      const p = arr[i];
      if (p.type === 'text' && p.state === 'streaming') return { id: p.id, text: p.text };
    }
    const last = arr[arr.length - 1];
    if (last && last.type === 'text' && last.text) return { id: last.id, text: last.text };
    return null;
  });

  /** The text content of the streaming zone. */
  let liveStreamingText = $derived(zoneTextPart?.text ?? "");

  // Current running tool — show any tool that hasn't completed yet.
  // Reads the effective parts list (streaming parts while live, saved
  // parts otherwise) so the label reflects real tool state regardless
  // of isProcessing (an agent that errored mid-tool still shows which
  // tool it was on, instead of a permanent "准备中").
  let runningToolLabel = $derived.by(() => {
    const source = (isLive ? streamingParts : parts) as UIMessagePart[];
    const tool = source
      .filter((p) => p.type === "tool-invocation")
      .filter((p) => {
        const t = p as ToolInvocationPart;
        const st = t.state as string;
        const hasResult = !!t.result;
        return st !== "output-available" && st !== "output-error"
          && (st !== "done" || !hasResult);
      })
      .slice(-1)[0] as ToolInvocationPart | undefined;
    if (tool) {
      const tKey = `toolname.${tool.name}`;
      const toolName = $t(tKey) !== tKey ? $t(tKey) : tool.name;
      return $t("thinking.toolRunning").replace("{tool}", toolName);
    }
    return "";
  });

  /** Show the "Running" pill in the header while the backend is
   *  actively working on this turn.  Delegates entirely to `isLive`
   *  which guarantees "latest assistant turn only". */
  let isRunning = $derived(isLive);

  /** The footer status pill + live timer driver.  This is the
   *  SINGLE source of truth for "is the backend still working on
   *  THIS turn?".  Delegates entirely to `isLive`. */
  let isBackendStillWorking = $derived(isLive);

  /** Live elapsed-time ticker. We use a module-level ticker (see
   *  `utils/ticker.ts`) that increments a shared $state proxy every
   *  1000ms for the lifetime of the page. This is reliable across
   *  component remounts: a per-component setInterval set up in
   *  onMount can be cleared if Svelte re-keys the each block, but
   *  the module-level interval is started once and never cleared.
   *  Each ChatMessage reads `tickerState.now` to compute the elapsed
   *  time for its own message.timestamp. */
  // Start the global ticker. The returned cleanup is a no-op (the
  // ticker runs for the page lifetime).
  useTicker();
  // Only live/streaming messages subscribe to the ticker. Historical
  // messages return a frozen value (their final `duration`) so a
  // 1s ticker write doesn't re-evaluate every ChatMessage in the
  // conversation.
  let liveElapsedMs = $derived.by(() => {
    if (!isLive && !message.streaming) return message.duration ?? 0;
    return message.timestamp
      ? Math.max(0, tickerState.now - new Date(message.timestamp).getTime())
      : 0;
  });

  let completedAt = $derived(
    !isLive && message.duration != null && message.timestamp
      ? new Date(new Date(message.timestamp).getTime() + message.duration).toISOString()
      : null
  );

  // ── Helpers ──

  function formatTime(iso: string): string {
    try {
      return new Date(iso).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
    } catch {
      return "";
    }
  }

  function formatDuration(ms: number): string {
    if (!Number.isFinite(ms) || ms < 0) return `0${tSync(lang, "message.duration.ms")}`;
    if (ms < 1000) return `${Math.round(ms)}${tSync(lang, "message.duration.ms")}`;
    const totalSec = Math.floor(ms / 1000);
    if (totalSec < 60) {
      return tSync(lang, "message.duration.formatSec").replace("{s}", (ms / 1000).toFixed(1));
    }
    const m = Math.floor(totalSec / 60);
    const s = totalSec % 60;
    return tSync(lang, "message.duration.format")
      .replace("{m}", String(m))
      .replace("{s}", String(s));
  }

  function parseEditArgs(args: string | undefined, toolName: string): { filePath: string; oldString: string; newString: string } {
    const fallback = { filePath: "", oldString: "", newString: "" };
    if (!args) return fallback;
    try {
      const parsed = JSON.parse(args);
      if (toolName === "write") {
        return {
          filePath: typeof parsed.file_path === "string" ? parsed.file_path : "",
          oldString: "",
          newString: typeof parsed.content === "string" ? parsed.content : "",
        };
      }
      return {
        filePath: typeof parsed.file_path === "string" ? parsed.file_path : "",
        oldString: typeof parsed.old_string === "string" ? parsed.old_string : "",
        newString: typeof parsed.new_string === "string" ? parsed.new_string : "",
      };
    } catch {
      return fallback;
    }
  }

  function roleLabel(role: string): string {
    switch (role) {
      case "user": return tSync(lang, "message.role.you");
      case "assistant": return tSync(lang, "message.role.agent");
      default: return role;
    }
  }

  /** Rough output-token estimate derived from the visible text/reasoning
   *  parts. Used as a fallback when the backend's `done` event was
   *  swallowed (e.g. Tauri `handle.abort()` killed the agent before the
   *  SSE event was emitted) so the footer still shows a number instead
   *  of "…". Mirrors `estimateTokens` in stores/types.ts. */
  let contentTokenEstimate = $derived.by<number | null>(() => {
    if (!hasFinished) return null;
    const text = parts
      .filter((p) => p.type === "text" || p.type === "reasoning")
      .map((p) => (p as { text: string }).text)
      .join("");
    if (!text) return null;
    return Math.ceil(text.length / 4);
  });

  let copied = $state(false);

  function copyContent() {
    // Use parts as primary source (cleaner, no thinking/tags)
    let text = parts.filter(p => p.type === 'text').map(p => p.text).join('');
    // Fallback to message.content for legacy sessions without parts
    if (!text) text = message.content;
    if (!text) return;
    navigator.clipboard.writeText(text).then(() => {
      copied = true;
      setTimeout(() => copied = false, 1600);
    }).catch(() => {
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      document.body.removeChild(ta);
      copied = true;
      setTimeout(() => copied = false, 1600);
    });
  }

  /** Merge consecutive text/reasoning parts for display (adjacent parts of same type).
   *  Filter out parts that render nothing to prevent blank event-item bubbles:
   *  - data parts (except user_intervention which has a card)
   *  - text parts with empty/whitespace-only content */
  let displayGroups = $derived.by(() => {
    const groups: UIMessagePart[] = [];
    for (const p of parts) {
      // Drop parts that would produce empty <div class="event-item"> bubbles
      if (p.type === "data" && p.dataType !== "user_intervention") continue;
      if (p.type === "text" && !p.text?.trim()) continue;
      const last = groups.length > 0 ? groups[groups.length - 1] : null;
      if (last && last.type === p.type && last.type !== 'tool-invocation') {
        if (p.type === 'text') {
          groups[groups.length - 1] = { ...last, text: (last as any).text + (p as any).text, state: p.state } as UIMessagePart;
        } else if (p.type === 'reasoning') {
          groups[groups.length - 1] = { ...last, text: (last as any).text + (p as any).text, state: p.state } as UIMessagePart;
        }
      } else {
        groups.push({ ...p } as UIMessagePart);
      }
    }
    return groups;
  });

  // ── Timeline folding (activity timeline collapse) ──
  const FOLD_THRESHOLD = 5;

  let timelineExpanded = $state(false);

  let totalDisplayItems = $derived(displayGroups.length);
  /** Fold whenever the visible card count exceeds the threshold —
   *  INCLUDING while the backend is still working. A long-running
   *  turn accumulates many tool cards; the oldest ones must collapse
   *  into the activity-timeline header as the turn grows, not only
   *  after it completes. Cards that are still running are kept
   *  visible (see `visibleGroups`) so their live "运行中" state is
   *  never hidden away mid-turn. */
  let hasOverflow = $derived(totalDisplayItems > FOLD_THRESHOLD);

  let foldedGroups = $derived(
    hasOverflow && !timelineExpanded
      ? displayGroups.filter((p) => !visibleGroups.includes(p))
      : []
  );

  let visibleGroups = $derived.by<UIMessagePart[]>(() => {
    if (!hasOverflow || timelineExpanded) return displayGroups;
    // Keep the newest FOLD_THRESHOLD cards...
    const tail = displayGroups.slice(-FOLD_THRESHOLD);
    const keepKeys = new Set(
      tail.map((p) => (p.type === 'tool-invocation' ? (p as ToolInvocationPart).toolCallId : p.id))
    );
    // ...plus any tool card still executing, so its live state stays
    // on screen even when the turn is long and running.
    const merged = displayGroups.filter((p) => {
      const key = p.type === 'tool-invocation' ? (p as ToolInvocationPart).toolCallId : p.id;
      if (keepKeys.has(key)) return true;
      if (p.type === 'tool-invocation') {
        const t = p as ToolInvocationPart;
        const st = t.state as string;
        const stillRunning = t.result == null && st !== "output-available" && st !== "output-error" && st !== "done";
        if (stillRunning) return true;
      }
      return false;
    });
    return merged;
  });

  /** 只有最后一个 reasoning part 才接收运行状态标签 —
   *  多个思考卡片不应同时显示"准备中/正在xx中"。 */
  let lastReasoningIdx = $derived.by(() => {
    for (let i = visibleGroups.length - 1; i >= 0; i--) {
      if (visibleGroups[i].type === "reasoning") return i;
    }
    return -1;
  });

  let foldedStats = $derived.by(() => {
    const folded = foldedGroups;
    let totalMs = 0;
    let toolCount = 0;
    let hasError = false;
    for (const p of folded) {
      if (p.type === "tool-invocation") {
        toolCount++;
        if ("durationMs" in p && typeof p.durationMs === "number") totalMs += p.durationMs;
        if (p.state === "output-error") hasError = true;
      }
    }
    return { count: folded.length, totalMs, toolCount, hasError };
  });

  function toggleTimeline() {
    timelineExpanded = !timelineExpanded;
  }

  function formatDurationCompact(ms: number): string {
    if (ms < 1000) return `${Math.round(ms)}${tSync(lang, "message.duration.ms")}`;
    const sec = ms / 1000;
    if (sec < 60) return tSync(lang, "message.duration.formatSec").replace("{s}", sec.toFixed(1));
    const m = Math.floor(sec / 60);
    const s = sec % 60;
    return tSync(lang, "message.duration.formatShort")
      .replace("{m}", String(m))
      .replace("{s}", s.toFixed(0));
  }

  /** True when a tool part is finished: has a result, reached an output
   *  terminal state, or was finalized ('done' — set by finalizeAssistantMessage
   *  even when the agent errored mid-tool with no result). */
  function isToolPartDone(p: UIMessagePart): boolean {
    if (p.type !== "tool-invocation") return false;
    const st = p.state as string;
    return p.result != null || st === "output-available" || st === "output-error" || st === "done";
  }
</script>

{#if message.role === "system"}
  <div class="message system">
    <div class="system-content">{message.content}</div>
  </div>
{:else}
  <div class="message-row" data-message-id={message.id} class:user={message.role === "user"} class:assistant={message.role === "assistant"} class:live={isLive}>
    <div class="bubble">
      <div class="bubble-header">
        {#if message.role === "user"}
          <span class="role-badge" class:user={message.role === "user"}>{roleLabel(message.role)}</span>
        {/if}
        {#if isRunning}
        {/if}
        <button class="copy-btn" onclick={copyContent} title={$t("chat.copy")}>
          {#if copied}
            <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
              <path d="M3 7.5l3 3 5-6" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
            </svg>
          {:else}
            <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
              <rect x="3.5" y="5" width="8" height="8" rx="1.5" stroke="currentColor" stroke-width="1.2"/>
              <path d="M10 5V3.5a1.5 1.5 0 00-1.5-1.5h-5A1.5 1.5 0 002 3.5v5A1.5 1.5 0 003.5 10H5" stroke="currentColor" stroke-width="1.2"/>
            </svg>
          {/if}
        </button>
        {#if canRegenerate}
          <button class="regenerate-btn" onclick={() => chat.regenerate()} title={$t("message.regenerateTitle")}>
            <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
              <path d="M2 5.5A5 5 0 0111.5 3.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
              <path d="M9 2.5l2.5 1-1 2.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/>
              <path d="M12 8.5A5 5 0 012.5 10.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
              <path d="M5 11.5l-2.5-1 1-2.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/>
            </svg>
          </button>
        {/if}
      </div>

      {#if message.role === "assistant"}
        {#if hasOverflow}
          <button class="timeline-header" onclick={toggleTimeline} type="button">
            <span class="timeline-label">
              {$t("message.timeline")} · 折叠 {foldedStats.count} 事 · {foldedStats.toolCount} 工具
              {#if !timelineExpanded && foldedStats.totalMs > 0} · {formatDurationCompact(foldedStats.totalMs)}{/if}
            </span>
            {#if foldedStats.hasError && !timelineExpanded}
              <span class="timeline-err">⚠ 1</span>
            {/if}
          </button>
        {/if}
        {#each visibleGroups as p, i (p.type === 'tool-invocation' ? p.toolCallId : p.id)}
          <div class="event-item">
            {#if p.type === "reasoning"}
              <ThinkingBlock
                thinking={p.text}
                duration={message.duration ?? 0}
                durationMs={("durationMs" in p) ? p.durationMs : undefined}
                streaming={isLive && p.state === "streaming"}
                showTimer={showTimer}
                runningTool={i === lastReasoningIdx ? runningToolLabel : ""}
                showPausedWarning={!isLive}
              />
            {:else if p.type === "tool-invocation" && (p.name === "edit" || p.name === "patch" || p.name === "write")}
              {@const editArgs = parseEditArgs(p.args, p.name)}
              <EditCard
                filePath={editArgs.filePath}
                oldString={editArgs.oldString}
                newString={editArgs.newString}
                durationMs={p.durationMs}
                result={p.result}
                completed={isToolPartDone(p)}
                showTimer={showTimer}
                workingDir={workingDir}
              />
            {:else if p.type === "tool-invocation" && isSkillOrMcpInvocation(p.name)}
              <SkillMcpCard
                toolCall={{ name: p.name, arguments: p.args, result: p.result }}
                durationMs={p.durationMs}
                completed={isToolPartDone(p)}
                showTimer={showTimer}
              />
            {:else if p.type === "tool-invocation"}
              <ToolCallCard
                toolCall={{ name: p.name, arguments: p.args, result: p.result }}
                durationMs={p.durationMs}
                completed={isToolPartDone(p)}
                showTimer={showTimer}
                hasError={p.state === "output-error"}
                workingDir={workingDir}
              />
            {:else if p.type === "data" && p.dataType === "user_intervention"}
              <div class="intervention-card">
                <div class="intervention-label">{$t("thinking.intervention")}</div>
                <div class="intervention-content">{p.content}</div>
              </div>
            {:else if p.type === "text" && p.text}
              <div class="markdown-content content-block">{@html renderMarkdown(p.text)}</div>
            {/if}
          </div>
        {/each}
        {#if isLive && liveStreamingText}
          <div class="bubble-content streaming-zone">
            <StreamingText text={liveStreamingText} />
          </div>
        {:else if !isLive && message.content && !parts.some((p) => p.type === "text" && p.text)}
          <div class="bubble-content">
            <div class="markdown-content">{@html renderMarkdown(message.content)}</div>
          </div>
        {/if}

        {#if message.exitReason && message.exitReason !== "end_turn" && message.exitReason !== "EXITED" && !isLive}
          <div class="exit-reason-banner">
            {$t("message.agentStopped")}: {$t(`exit.${message.exitReason}`, message.exitReason)}
          </div>
        {/if}

        {#if !isBackendStillWorking}
          <div class="bubble-footer" class:running={isBackendStillWorking}>
            <span class="footer-time-group" title="System time when this turn started">
              <span class="footer-inscription">{roleLabel(message.role)}{$t("message.sig", "识")}</span>
              <span class="footer-time-text">
                {#if completedAt}
                  {formatTime(completedAt)}
                {:else if message.timestamp}
                  {formatTime(message.timestamp)}
                {/if}
              </span>
            </span>
            <span class="footer-sep" aria-hidden="true"></span>
            <span class="footer-duration-group" title={$t("message.totalTitle")}>
              <svg class="footer-icon" width="11" height="11" viewBox="0 0 12 12" fill="none" aria-hidden="true">
                <path d="M6 2.5l4 2.5-4 2.5M6 7.5l-4 2.5 4-2.5" stroke="currentColor" stroke-width="1" stroke-linecap="round" stroke-linejoin="round"/>
              </svg>
              <span class="footer-time-text">
                <span class="footer-stat-label">{$t("message.total")}</span>
                {#if message.duration != null && message.duration > 0}
                  {formatDuration(message.duration)}
                {:else if message.timestamp}
                  {formatDuration(liveElapsedMs)}
                {/if}
              </span>
            </span>
          </div>
        {/if}

      {:else}
        <!-- User message -->
        <div class="bubble-content">
          <div class="markdown-content">{@html renderMarkdown(message.content, { highlight: false })}</div>
        </div>
        {#if message.timestamp}
          <div class="bubble-footer user-footer">
            <span class="footer-time-group" title="System time">
              <span class="footer-inscription">{roleLabel(message.role)}{$t("message.sig", "识")}</span>
              <span class="footer-time-text">{formatTime(message.timestamp)}</span>
            </span>
          </div>
        {/if}
      {/if}
    </div>
  </div>
{/if}

<style>
  .message-row {
    display: flex;
    padding: 4px 16px;
    max-width: 100%;
    min-width: 0;
    position: relative;
    /* Skip layout/paint for off-viewport messages. Long sessions
       re-render every ChatMessage on every store update (streaming
       deltas, ticker); content-visibility restricts rendering to
       the viewport band, so historical rows are inert. The intrinsic
       size placeholder keeps scrollbar geometry stable. */
    content-visibility: auto;
    contain-intrinsic-size: auto 120px;
  }
  /* The streaming row must render live: content-visibility would
     substitute the 120px placeholder for the real height while the
     agent streams, making the cursor jump and cards below shift
     ("text far from the thinking card, then snapping closer"). */
  .message-row.live {
    content-visibility: visible;
    contain-intrinsic-size: auto;
  }
  .message-row.user {
    justify-content: flex-end;
  }
  .message-row.assistant {
    justify-content: flex-start;
  }
  .message.system {
    text-align: center;
    padding: 8px 16px;
  }
  .system-content {
    font-size: 12px;
    color: var(--text-secondary);
    font-style: italic;
    background: var(--bg-tertiary);
    display: inline-block;
    padding: 4px 12px;
    border-radius: 8px;
    max-width: 80%;
  }
  .bubble {
    width: auto;
    min-width: 0;
    border-radius: 3px;
    padding: 8px 14px;
    border: 1px solid transparent;
    font-size: 14px;
    box-sizing: border-box;
  }
  .message-row.assistant .bubble {
    /* 纸上墨: 无容器、无底色、纯墨字 */
    background: transparent;
    border: none;
    border-radius: 0;
    max-width: clamp(360px, 78%, 720px);
  }
  .message-row.user .bubble {
    /* 釉色条: 整行淡青底 + 左缘青线 (器物刻痕) */
    background: var(--color-primary-muted, rgba(147,195,214,0.07));
    border-left: 2px solid var(--color-primary, #93c3d6);
    border-radius: 2px;
    max-width: clamp(280px, 68%, 620px);
  }

  @media (max-width: 1100px) {
    .message-row.assistant .bubble { max-width: 88%; }
    .message-row.user .bubble { max-width: 78%; }
  }
  @media (max-width: 720px) {
    .message-row { padding: 4px 10px; }
    .message-row.assistant .bubble,
    .message-row.user .bubble { max-width: 100%; }
  }
  @media (min-width: 1600px) {
    .message-row.assistant .bubble { max-width: clamp(480px, 56%, 1100px); }
    .message-row.user .bubble { max-width: clamp(360px, 48%, 900px); }
  }
  .bubble-header {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 6px;
    flex-wrap: wrap;
  }
  .role-badge {
    /* 铭文: 宋体小标 */
    font-family: var(--font-serif);
    font-size: 11px;
    letter-spacing: 0.25em;
    padding: 0;
    background: none;
    color: var(--text-secondary);
  }
  .role-badge.user {
    color: var(--color-primary);
  }
  .role-badge.assistant {
    color: var(--color-accent);
  }
  .status-pill {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.4px;
    padding: 2px 7px;
    border-radius: 999px;
    background: var(--bg-tertiary);
    color: var(--text-tertiary);
  }
  .status-pill.running {
    color: #f59e0b;
    background: rgba(245, 158, 11, 0.10);
  }
  .status-pill.done {
    color: #22c55e;
    background: rgba(34, 197, 94, 0.10);
  }
  .status-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: #f59e0b;
    box-shadow: 0 0 0 0 rgba(245, 158, 11, 0.7);
    animation: statusPulse 1.4s ease-out infinite;
  }
  @keyframes statusPulse {
    0%   { box-shadow: 0 0 0 0 rgba(245, 158, 11, 0.7); }
    70%  { box-shadow: 0 0 0 6px rgba(245, 158, 11, 0); }
    100% { box-shadow: 0 0 0 0 rgba(245, 158, 11, 0); }
  }
  .copy-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    background: none;
    border: none;
    color: var(--text-tertiary);
    cursor: pointer;
    padding: 2px;
    border-radius: 4px;
    margin-left: auto;
    opacity: 0.4;
    transition: opacity 0.15s;
  }
  .copy-btn:hover {
    opacity: 1;
  }
  .regenerate-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    background: none;
    border: none;
    color: var(--color-primary);
    cursor: pointer;
    padding: 2px;
    border-radius: 4px;
    opacity: 0.3;
    transition: opacity 0.15s, transform 0.3s;
  }
  .regenerate-btn:hover {
    opacity: 1;
    transform: rotate(45deg);
  }
  .content-block {
    padding: 4px 0;
  }
  .bubble-content {
    line-height: 1.6;
    max-width: 70ch;
    overflow-wrap: break-word;
    word-break: break-word;
  }
  .streaming-zone {
    /* 光标/打字点紧跟最后一张卡片, 不再预留空行 */
    min-height: 0;
  }
  .markdown-content {
    line-height: 1.7;
    overflow-wrap: break-word;
    word-break: break-word;
    hanging-punctuation: allow-end;
  }
  .markdown-content :global(p) {
    margin: 0.5em 0;
    text-indent: 0;
  }
  .markdown-content :global(p:first-child) {
    margin-top: 0;
  }
  .markdown-content :global(p:last-child) {
    margin-bottom: 0;
  }
  .markdown-content :global(pre) {
    margin: 8px 0;
    padding: 12px;
    border-radius: 8px;
    background: var(--bg-tertiary);
    overflow-x: auto;
    overflow-wrap: normal;
    word-break: normal;
    white-space: pre;
    font-size: 13px;
    max-width: 100%;
  }
  .markdown-content :global(code) {
    font-family: var(--font-mono, "JetBrains Mono", monospace);
    font-size: 0.9em;
    overflow-wrap: anywhere;
  }
  .markdown-content :global(img) {
    max-width: 100%;
    border-radius: 8px;
  }
  .markdown-content :global(blockquote) {
    border-left: 3px solid var(--color-hairline);
    margin: 8px 0;
    padding: 4px 12px;
    color: var(--text-secondary);
  }
  .markdown-content :global(table) {
    border-collapse: collapse;
    width: 100%;
    margin: 8px 0;
    font-size: 13px;
  }
  .markdown-content :global(th),
  .markdown-content :global(td) {
    border: 1px solid var(--color-hairline);
    padding: 6px 10px;
    text-align: left;
  }
  .markdown-content :global(th) {
    background: var(--bg-tertiary);
    font-weight: 600;
  }
  .markdown-content :global(.math-inline) {
    font-family: var(--font-mono, "JetBrains Mono", "CMU Serif", monospace);
    background: var(--color-surface-soft);
    padding: 1px 6px;
    border-radius: 4px;
    font-size: 0.95em;
  }
  .markdown-content :global(.md-link) {
    color: var(--color-accent, #5b9bd5);
    text-decoration: underline;
    text-underline-offset: 2px;
    text-decoration-thickness: 1px;
    text-decoration-color: color-mix(in srgb, var(--color-accent, #5b9bd5) 40%, transparent);
    transition: text-decoration-color 0.15s;
  }
  .markdown-content :global(.md-link:hover) {
    text-decoration-color: var(--color-accent, #5b9bd5);
  }
  .markdown-content :global(.math-block) {
    font-family: var(--font-mono, "JetBrains Mono", "CMU Serif", monospace);
    background: var(--color-surface-soft);
    padding: 12px 16px;
    border-radius: 8px;
    margin: 8px 0;
    overflow-x: auto;
    white-space: pre;
    font-size: 0.95em;
    text-align: center;
  }
  .event-item {
    margin: 4px 0;
  }
  .timeline-header {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 6px 12px;
    margin: 6px 0 2px;
    border: 1px dashed var(--color-primary);
    background: var(--color-primary-muted);
    color: var(--color-primary);
    cursor: pointer;
    font-family: var(--font-serif);
    font-size: 12px;
    letter-spacing: 0.08em;
    text-align: left;
    border-radius: 3px;
    transition: color 0.35s var(--ease-soak, ease), background 0.35s var(--ease-soak, ease);
  }
  .timeline-header:hover {
    color: var(--color-primary-hover);
    background: var(--color-primary-muted);
    border-color: var(--color-primary-hover);
  }
  .timeline-label {
    font-weight: 500;
    font-variant-numeric: tabular-nums;
  }
  .timeline-err {
    margin-left: auto;
    color: var(--color-error, #c05a3e);
    font-size: 10.5px;
    font-family: var(--font-mono);
  }
  .exit-reason-banner {
    margin-top: 8px;
    padding: 6px 10px;
    border-radius: 6px;
    font-size: 12px;
    background: var(--bg-warning, #3a2d00);
    color: var(--text-warning, #ffd966);
    border: 1px solid var(--border-warning, #5c4800);
    line-height: 1.4;
  }
  .bubble-footer {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 8px;
    padding-top: 6px;
    border-top: 1px dashed var(--color-hairline, #1d1d1f);
    font-size: 11px;
    color: var(--text-tertiary);
    flex-wrap: wrap;
  }
  .bubble-footer.user-footer {
    border-top: none;
    justify-content: flex-end;
    margin-top: 4px;
    padding-top: 0;
  }
  .bubble-footer.running {
    border-top-color: rgba(245, 158, 11, 0.25);
  }
  .footer-sep {
    width: 1px;
    height: 10px;
    background: var(--color-hairline, #1d1d1f);
    opacity: 0.6;
  }
  .footer-time-group,
  .footer-duration-group {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .footer-icon {
    flex: 0 0 auto;
    color: var(--text-bright);
    transform: translateY(0);
  }
  .footer-inscription {
    /* 落款: 某某识 */
    font-family: var(--font-serif);
    font-size: 10px;
    letter-spacing: 0.18em;
    color: var(--text-tertiary);
  }
  .footer-time-text {
    font-variant-numeric: tabular-nums;
    line-height: 1;
    color: var(--text-bright);
  }
  .footer-stat-label {
    font-size: 9px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-tertiary);
    margin-right: 2px;
    font-weight: 500;
  }
  .footer-token {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .token-pill {
    display: inline-flex;
    align-items: baseline;
    gap: 3px;
    padding: 1px 6px;
    border-radius: 5px;
    font-variant-numeric: tabular-nums;
    line-height: 1.1;
    border: 1px solid transparent;
  }
  .token-pill-out {
    background: color-mix(in srgb, var(--color-accent) 14%, transparent);
    border-color: color-mix(in srgb, var(--color-accent) 32%, transparent);
    color: var(--color-accent);
  }
  .token-pill-in {
    background: color-mix(in srgb, #8b5cf6 12%, transparent);
    border-color: color-mix(in srgb, #8b5cf6 30%, transparent);
    color: #a78bfa;
  }
  .token-pill-num {
    font-weight: 700;
    font-size: 11px;
    letter-spacing: 0.01em;
  }
  .token-pill-label {
    font-size: 9px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    opacity: 0.85;
  }

  /* ── User Intervention Card ── */
  .intervention-card {
    margin: 6px 0;
    border-radius: 8px;
    border: 1px solid #92400e;
    background: rgba(146, 64, 14, 0.08);
    padding: 10px 12px;
  }
  .intervention-label {
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: #d97706;
    margin-bottom: 4px;
  }
  .intervention-content {
    font-size: 13px;
    line-height: 1.5;
    color: var(--color-ink);
    white-space: pre-wrap;
    word-break: break-word;
  }
</style>
