  <script lang="ts">
  import { onMount, untrack } from "svelte";
  import { qualityContract } from "../stores/types";
  import ToolCallCard from "./ToolCallCard.svelte";
  import ThinkingBlock from "./ThinkingBlock.svelte";
  import EditCard from "./EditCard.svelte";
  import SkillMcpCard from "./SkillMcpCard.svelte";
  import { submitMessageFeedback } from "../api/chat";
  import { sessions } from "../stores/sessions";
  import type { UIMessagePart, ToolInvocationPart } from "../stores/parts";
  import { convertStreamEventsToParts } from "../stores/parts";
  import { chat } from "../stores/chat";
  import { mathReady, renderMarkdown } from "../utils/markdown";
  import { tickerState, useTicker } from "../utils/ticker.svelte";
import ChangedFilesCard from "./ChangedFilesCard.svelte";
import { collectTurnFiles, isFileEditTool, parseEditArgs, type TurnFile } from "../utils/fileChanges";
import { t, locale, tSync } from "../i18n";
  let lang = $state("zh");
  $effect(() => { lang = $locale; });


  // Stable empty array for non-live messages: historical bubbles never
  // receive a new array reference when streaming parts change.
  const NO_STREAMING_PARTS: UIMessagePart[] = [];
  // Same idea for the changed-files card: a turn that is still running
  // renders no card, and returning one shared empty array keeps that from
  // invalidating anything downstream on every streaming delta.
  const NO_CHANGED_FILES: TurnFile[] = [];

  let {
    message,
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
   *  parts otherwise.  Live text parts render at their OWN array
   *  position via StreamingText (see the template) — there is no
   *  trailing zone anymore, so nothing is excluded here. */
  let parts = $derived.by<UIMessagePart[]>(() => {
    if (isLive) return streamingParts;
    if (message.parts && message.parts.length > 0) return message.parts;
    if (message.streamEvents && message.streamEvents.length > 0) return convertStreamEventsToParts(message.streamEvents);
    return [];
  });

  /** Show the "Running" pill in the header while the backend is
   *  actively working on this turn.  Delegates entirely to `isLive`
   *  which guarantees "latest assistant turn only". */
  /** The footer status pill + live timer driver.  This is the
   *  SINGLE source of truth for "is the backend still working on
   *  THIS turn?".  Delegates entirely to `isLive`. */
  let isBackendStillWorking = $derived(isLive);

  /** Normal endings don't deserve a warning tone in the footer label. */
  // Normal endings must not be tinted as warnings. The backend's most
  // common value is `EXITED`, and a respond-path finish without an explicit
  // reason lands on `CURRENT_TASK_DONE` — both used to be flagged abnormal
  // (review findings B-3 / E-6).
  const NORMAL_EXIT_REASONS = new Set([
    "end_turn",
    "EXITED",
    "CURRENT_TASK_DONE",
    "completed",
  ]);
  let isAbnormalExit = $derived(
    !!message.exitReason && !NORMAL_EXIT_REASONS.has(message.exitReason)
  );

  // Delivery-gate state. Always rendered (never hover-only): "not verified"
  // must be as visible as "passed", otherwise an unchecked delivery reads as
  // a checked one (R5-5).
  let contract = $derived(qualityContract(message.quality));

  // ── P2-28 user feedback ──
  let feedback = $state<"up" | "down" | null>(null);
  let feedbackNote = $state("");
  let noteOpen = $state(false);
  let feedbackBusy = $state(false);
  let feedbackError = $state("");

  async function sendFeedback(verdict: "up" | "down") {
    const sid = $sessions.currentId;
    if (!sid) return;
    feedbackBusy = true;
    feedbackError = "";
    try {
      await submitMessageFeedback(
        sid,
        message.serverIndex ?? -1,
        verdict,
        verdict === "down" ? feedbackNote.trim() : "",
      );
      feedback = verdict;
      noteOpen = false;
    } catch (e) {
      feedbackError = e instanceof Error ? e.message : String(e);
    } finally {
      feedbackBusy = false;
    }
  }

  /** What this turn produced, aggregated across every edit/patch/write —
   *  plus the deliverables the agent announced with `open_side_panel`, which
   *  are the only evidence a file exists at all when the agent built it with
   *  a `code_run` script.  Including the ones the timeline fold hides.
   *  Gated on the turn being over: a running turn shows no card (it would
   *  still be growing), and nothing pays for the diff while tokens are
   *  streaming.
   *
   *  `collectTurnFiles` caches on the parts array, so this recomputes only
   *  when the rendered part list actually changes. */
  let changedFiles = $derived.by<TurnFile[]>(() => {
    if (isBackendStillWorking) return NO_CHANGED_FILES;
    return collectTurnFiles(parts, workingDir);
  });

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

  /** Wall-clock time this turn finished, or null when the turn is live or
   *  carries no timing at all.
   *
   *  `message.completedAt` is authoritative (set at finalize for live turns,
   *  and by the session parser for turns restored from disk, where the saved
   *  `timestamp` already IS the finish time). Only fall back to
   *  `timestamp + duration` — correct solely for an in-memory turn whose
   *  timestamp is its start — so a restored 34-minute turn no longer displays
   *  a time 34 minutes in the future. */
  let completedAt = $derived.by(() => {
    if (isLive) return null;
    if (message.completedAt) return message.completedAt;
    if (message.duration != null && message.timestamp) {
      return new Date(new Date(message.timestamp).getTime() + message.duration).toISOString();
    }
    return null;
  });

  // ── Helpers ──

  function formatTime(iso: string): string {
    try {
      // Follow the APP locale (not the OS default) so the timestamp has
      // the same hour cycle/format as the rest of the UI.
      return new Date(iso).toLocaleTimeString($locale, { hour: "2-digit", minute: "2-digit" });
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
      // Same for reasoning: an empty reasoning part renders a 0px wrapper,
      // but it still COUNTS toward the fold window — the window then evicts
      // its oldest group one step BEFORE the new thinking block has any
      // height, so the live bubble collapses and refills at every cycle
      // boundary (the per-cycle -33px downward blip while thinking).
      if (p.type === "reasoning" && !p.text?.trim()) continue;
      const last = groups.length > 0 ? groups[groups.length - 1] : null;
      // Only text/reasoning parts merge. The type-equality guard MUST be
      // scoped to them: a data part (user_intervention card) adjacent to
      // another data part — two interjections queued during one long tool
      // call — used to enter this branch, match neither merge case, and be
      // silently dropped, so only the FIRST interjection ever rendered.
      if (
        last
        && (p.type === "text" || p.type === "reasoning")
        && last.type === p.type
      ) {
        if (p.type === 'text') {
          groups[groups.length - 1] = { ...last, text: (last as any).text + (p as any).text, state: p.state } as UIMessagePart;
        } else {
          groups[groups.length - 1] = { ...last, text: (last as any).text + (p as any).text, state: p.state } as UIMessagePart;
        }
      } else {
        groups.push({ ...p } as UIMessagePart);
      }
    }
    return groups;
  });

  // ── Single-line run status (DSH-style) ──
  //
  // While a turn is live the bubble shows ONE line: what the agent is doing
  // right now. Tool/thinking cards are no longer kept on screen (the old
  // "last 5 cards" fold), and the finished bubble shows "已完成，用时 …"
  // followed by the final reply only.

  let detailsOpen = $state(false);

  // Only the LAST text segment is the final reply. Earlier text is the
  // model talking between tools ("let me check…") and must fold into the
  // details timeline with the tool cards, in chronological order — joining
  // every text part made the intermediate chatter repeat inside the answer.
  let finalTextIndex = $derived.by(() => {
    for (let i = displayGroups.length - 1; i >= 0; i--) {
      const p = displayGroups[i];
      if (p.type === "text" && p.text.trim()) return i;
    }
    return -1;
  });

  let detailGroups = $derived(
    displayGroups.filter((_, i) => i !== finalTextIndex),
  );

  let detailKeys = $derived.by(() => {
    const seen = new Set<string>();
    return detailGroups.map((p, i) => {
      const base = p.type === "tool-invocation" ? (p as ToolInvocationPart).toolCallId : p.id;
      let key = base || `part_${i}`;
      if (seen.has(key)) {
        let n = 2;
        while (seen.has(`${key}#${n}`)) n += 1;
        key = `${key}#${n}`;
      }
      seen.add(key);
      return key;
    });
  });

  function isSkillOrMcpInvocation(name: string): boolean {
    return name.startsWith("mcp__") || name === "skill_mcp_search" || name === "skill_mcp_list";
  }

  let finalText = $derived.by(() => {
    if (finalTextIndex >= 0) {
      const p = displayGroups[finalTextIndex];
      if (p.type === "text" && p.text) return p.text;
    }
    return message.role === "assistant" ? message.content ?? "" : "";
  });

  let completedLine = $derived(
    message.duration != null && message.duration > 0
      ? tSync(lang, "message.completedIn").replace("{d}", formatLongDuration(message.duration))
      : tSync(lang, "message.completed"),
  );

  // Parallel tool calls: show the NEWEST still-running tool only. Do not
  // stop at a newer text/reasoning part — with concurrent calls the newest
  // part is often streamed text while an earlier tool is still executing,
  // and the bubble must name that tool, not fall back to "thinking".
  let activityLabel = $derived.by(() => {
    for (let i = displayGroups.length - 1; i >= 0; i--) {
      const p = displayGroups[i];
      if (p.type === "tool-invocation" && !isToolPartDone(p)) {
        return toolActivity(p as ToolInvocationPart);
      }
    }
    return tSync(lang, "message.thinking");
  });

  function toolActivity(p: ToolInvocationPart): string {
    const key = `toolname.${p.name}`;
    const translated = tSync(lang, key);
    const label = translated === key ? p.name : translated;
    let args: Record<string, unknown> = {};
    try {
      args = (typeof p.args === "string" ? JSON.parse(p.args) : p.args) as Record<string, unknown>;
    } catch {
      args = {};
    }
    const path = args.file_path ?? args.path ?? args.filename;
    if (typeof path === "string" && path) {
      return `${label} ${path.split("/").pop()}`;
    }
    const detail = args.pattern ?? args.query ?? args.description;
    if (typeof detail === "string" && detail.trim()) {
      return `${label} ${detail.trim().slice(0, 42)}`;
    }
    return label;
  }

  function formatLongDuration(ms: number): string {
    const total = Math.max(0, Math.round(ms / 1000));
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const sec = total % 60;
    if (h > 0) {
      return tSync(lang, "message.duration.hourSec")
        .replace("{h}", String(h))
        .replace("{m}", String(m))
        .replace("{s}", String(sec));
    }
    if (m > 0) {
      return tSync(lang, "message.duration.minSec")
        .replace("{m}", String(m))
        .replace("{s}", String(sec));
    }
    return tSync(lang, "message.duration.secs").replace("{s}", String(sec));
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
        <button class="copy-btn" onclick={copyContent} title={$t("chat.copy")} aria-label={$t("chat.copy")}>
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
          <button class="regenerate-btn" onclick={() => chat.regenerate()} title={$t("message.regenerateTitle")} aria-label={$t("message.regenerateTitle")}>
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
        <div class="a-head">{roleLabel(message.role)}</div>
        {#snippet detailsPanel()}
          <div class="run-details">
            {#each detailGroups as p, partIdx (detailKeys[partIdx])}
              <div class="event-item">
                {#if p.type === "reasoning"}
                  <ThinkingBlock
                    thinking={p.text}
                    duration={message.duration ?? 0}
                    durationMs={("durationMs" in p) ? p.durationMs : undefined}
                    streaming={isLive && p.state === "streaming"}
                    showTimer={false}
                    showPausedWarning={!isLive}
                  />
                {:else if p.type === "tool-invocation" && isFileEditTool(p.name)}
                  {@const editArgs = parseEditArgs(p.args, p.name)}
                  <EditCard
                    filePath={editArgs?.filePath ?? ""}
                    oldString={editArgs?.oldString ?? ""}
                    newString={editArgs?.newString ?? ""}
                    durationMs={p.durationMs}
                    result={p.result}
                    completed={isToolPartDone(p)}
                    showTimer={false}
                    workingDir={workingDir}
                  />
                {:else if p.type === "tool-invocation" && isSkillOrMcpInvocation(p.name)}
                  <SkillMcpCard
                    toolCall={{ name: p.name, arguments: p.args, result: p.result }}
                    durationMs={p.durationMs}
                    completed={isToolPartDone(p)}
                    showTimer={false}
                  />
                {:else if p.type === "tool-invocation"}
                  <ToolCallCard
                    toolCall={{ name: p.name, arguments: p.args, result: p.result }}
                    durationMs={p.durationMs}
                    liveElapsedSecs={p.liveElapsedSecs}
                    completed={isToolPartDone(p)}
                    showTimer={false}
                    hasError={p.state === "output-error"}
                    workingDir={workingDir}
                  />
                {:else if p.type === "text"}
                  <div class="intermediate-text markdown-content">{@html renderMarkdown(p.text, { mathVersion: $mathReady })}</div>
                {:else if p.type === "data" && p.dataType === "user_intervention"}
                  <div class="intervention-card">
                    <div class="intervention-label">{$t("thinking.intervention")}</div>
                    <div class="intervention-content">{p.content}</div>
                  </div>
                {/if}
              </div>
            {/each}
          </div>
        {/snippet}

        {#if isLive}
          <button
            type="button"
            class="run-line"
            class:static={detailGroups.length === 0}
            aria-expanded={detailGroups.length > 0 ? detailsOpen : undefined}
            aria-live="polite"
            onclick={() => { if (detailGroups.length > 0) detailsOpen = !detailsOpen; }}
          >
            <span class="run-prompt" aria-hidden="true">&gt;_</span>
            <span class="run-state">{$t("message.executing")}</span>
            <span class="run-activity">{activityLabel}</span>
            <span class="run-cursor" aria-hidden="true"></span>
            {#if detailGroups.length > 0}
              <span class="run-chevron" aria-hidden="true">{detailsOpen ? "▴" : "▾"}</span>
            {/if}
          </button>
          {#if detailsOpen}
            {@render detailsPanel()}
          {/if}
        {:else}
          <button
            type="button"
            class="run-line done"
            class:static={detailGroups.length === 0}
            aria-expanded={detailGroups.length > 0 ? detailsOpen : undefined}
            onclick={() => { if (detailGroups.length > 0) detailsOpen = !detailsOpen; }}
          >
            <span class="run-check" aria-hidden="true">✓</span>
            <span>{completedLine}</span>
            {#if detailGroups.length > 0}
              <span class="run-chevron" aria-hidden="true">{detailsOpen ? "▴" : "▾"}</span>
            {/if}
          </button>
          {#if detailsOpen}
            {@render detailsPanel()}
          {/if}
          {#if finalText}
            <div class="markdown-content content-block">{@html renderMarkdown(finalText, { mathVersion: $mathReady })}</div>
          {/if}
        {/if}
        {#if message.exitReason && message.exitReason !== "end_turn" && message.exitReason !== "EXITED" && !isLive}
          <div class="exit-reason-banner">
            {$t("message.agentStopped")}: {$t(`exit.${message.exitReason}`, message.exitReason)}
          </div>
        {/if}

        {#if changedFiles.length > 0}
          <!-- Bottom of the bubble: what this turn produced — deliverables
               first, then everything it changed on disk — with a 打开 button
               that previews each file in the side panel. -->
          <ChangedFilesCard rows={changedFiles} {workingDir} />
        {/if}

        {#if !isBackendStillWorking}
          <div class="bubble-footer" class:running={isBackendStillWorking}>
            <span class="footer-time-group" title="System time when this turn finished">
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
            {#if message.exitReason}
              <!-- Always visible, not only on abnormal endings: "why did it
                   stop" is one of the three questions a long task must answer
                   without the user opening anything. -->
              <span class="footer-sep" aria-hidden="true"></span>
              <span
                class="footer-exit"
                class:warn={isAbnormalExit}
                title={$t("message.exitReasonTitle")}
              >
                {$t(`exit.${message.exitReason}`, message.exitReason)}
              </span>
            {/if}
            {#if !isLive}
              <!-- P2-28 feedback: thumbs are training signal, not decoration. -->
              <span class="footer-sep" aria-hidden="true"></span>
              {#if feedback}
                <span class="footer-feedback-done" role="status">
                  <span class="quality-dot" aria-hidden="true"></span>
                  {$t("feedback.recorded", "已记录")}
                </span>
              {:else}
                <span class="footer-feedback">
                  <button
                    class="fb-btn"
                    onclick={() => sendFeedback("up")}
                    disabled={feedbackBusy}
                    aria-label={$t("feedback.good", "这条回答有用")}
                    title={$t("feedback.good", "这条回答有用")}
                  >▲</button>
                  <button
                    class="fb-btn"
                    class:on={noteOpen}
                    onclick={() => (noteOpen = !noteOpen)}
                    disabled={feedbackBusy}
                    aria-label={$t("feedback.bad", "这条回答有问题")}
                    title={$t("feedback.bad", "这条回答有问题")}
                  >▼</button>
                </span>
              {/if}
              <!-- Delivery contract: three distinguishable states. -->
              <span class="footer-sep" aria-hidden="true"></span>
              <span
                class="footer-quality"
                class:ok={contract.tone === "ok"}
                class:warn={contract.tone === "warn"}
                class:muted={contract.tone === "muted"}
                title={message.quality?.note ?? $t(`${contract.labelKey}Title`)}
                data-review-status={contract.status}
              >
                <span class="quality-dot" aria-hidden="true"></span>
                {$t(contract.labelKey)}
              </span>
            {/if}
          </div>
          {#if noteOpen && !feedback}
            <div class="feedback-note">
              <textarea
                class="feedback-input"
                bind:value={feedbackNote}
                rows="2"
                placeholder={$t("feedback.notePlaceholder", "哪里不对？（可选，会用来修正技能/SOP 的评分）")}
              ></textarea>
              <div class="feedback-actions">
                <button class="fb-send" onclick={() => sendFeedback("down")} disabled={feedbackBusy}>
                  {$t("feedback.submit", "提交")}
                </button>
                <button class="fb-cancel" onclick={() => (noteOpen = false)}>
                  {$t("settings.close")}
                </button>
              </div>
            </div>
          {/if}
          {#if feedbackError}
            <div class="feedback-error" role="alert">{feedbackError}</div>
          {/if}
        {/if}

      {:else}
        <!-- User message -->
        <div class="bubble-content">
          <div class="markdown-content">{@html renderMarkdown(message.content, { highlight: false, mathVersion: $mathReady })}</div>
        </div>
        {#if message.timestamp}
          <div class="bubble-footer user-footer">
            <span class="footer-time-group" title="System time">
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
    padding: 0;
    max-width: 100%;
    min-width: 0;
    position: relative;
    /* NOTE: content-visibility: auto is deliberately NOT used here. The
       virtual window in App.svelte already bounds the mounted row count;
       layering cv on top makes a mounted-but-out-of-band row report its
       180px contain-intrinsic placeholder instead of its real height, so
       the spacer geometry (rowHeights) can never agree with the layout.
       The mismatch flips the virtual window boundary every frame during
       streaming — the high-frequency vertical flicker in long sessions. */
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
    border-radius: 0;
    padding: 0;
    border: none;
    font-size: 14.5px;
    line-height: 1.78;
    box-sizing: border-box;
  }
  .message-row.assistant .bubble {
    /* 纸上墨: 无容器、无底色、纯墨字。
       气泡随消息列伸展（可达 100%），长回复少折行；
       短回复时 flex item 仍按内容收缩, 视觉不变 */
    background: transparent;
    border: none;
    border-radius: 0;
    max-width: 100%;
  }
  /* 用户话: 右侧一块釉面玉牌 —— 圆角收右下角, 天青薄釉, 1px 高光边 */
  .message-row.user {
    justify-content: flex-end;
    padding: 2px 0 2px 15%;
  }
  .message-row.user .bubble {
    background: linear-gradient(
      180deg,
      color-mix(in srgb, var(--color-primary) 13%, var(--color-surface-elevated)),
      color-mix(in srgb, var(--color-primary) 7%, var(--color-surface-soft))
    );
    border: 1px solid color-mix(in srgb, var(--color-primary) 20%, transparent);
    border-radius: 16px 16px 5px 16px;
    padding: 10px 16px 8px;
    max-width: min(100%, 680px);
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.045);
  }

  @media (max-width: 720px) {
    .message-row { padding: 4px 10px; }
  }
  .bubble-header {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 2px;
    flex-wrap: wrap;
    min-height: 16px;
  }
  /* 用户气泡: 操作图标浮在右上角, 不占正文空间 */
  .message-row.user .bubble {
    position: relative;
  }
  .message-row.user .bubble-header {
    position: absolute;
    top: 4px;
    right: 6px;
    margin: 0;
    min-height: 0;
    z-index: 1;
  }
  .message-row.user .copy-btn {
    background: var(--color-surface-elevated);
    border: 1px solid var(--color-hairline);
  }

  /* 助手标识: 一小行小字 + 圆点 (预览 .ahead) */
  .a-head {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 9.5px;
    font-weight: 600;
    letter-spacing: 0.1em;
    color: var(--color-muted);
    margin-bottom: 4px;
  }
  .a-head::before {
    content: "";
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: currentColor;
    opacity: 0.8;
  }
  .role-badge {
    font-family: var(--font-sans);
    font-size: 9.5px;
    font-weight: 600;
    letter-spacing: 0.1em;
    padding: 0;
    background: none;
    color: var(--color-muted);
  }
  /* The .status-pill / .status-pill.running / .status-pill.done rules that
     lived here were dead — no markup or class directive ever applied
     .status-pill (the build warned "Unused CSS selector"). Removed 2026-10-23. */
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
    color: var(--color-muted);
    cursor: pointer;
    padding: 4px;
    border-radius: 7px;
    margin-left: auto;
    opacity: 0;
    transition: opacity 0.15s, background 0.15s, color 0.15s;
  }
  .bubble:hover .copy-btn,
  .copy-btn:focus-visible {
    opacity: 1;
  }
  .copy-btn:hover {
    background: var(--color-surface-soft);
    color: var(--color-ink);
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
    border-radius: 8px;
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
    /* 文本宽度跟随气泡（气泡自身已受消息列约束），
       不再叠加 70ch 二次上限，长回复可以铺满气泡宽度 */
    max-width: 100%;
    overflow-wrap: break-word;
    word-break: break-word;
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
    border-radius: 8px;
    font-size: 0.95em;
  }
  .markdown-content :global(.md-link) {
    color: var(--color-accent);
    text-decoration: underline;
    text-underline-offset: 2px;
    text-decoration-thickness: 1px;
    text-decoration-color: color-mix(in srgb, var(--color-accent) 40%, transparent);
    transition: text-decoration-color 0.15s;
  }
  .markdown-content :global(.md-link:hover) {
    text-decoration-color: var(--color-accent);
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
  /* DSH-style single status line: one row while running, one "done" row
     with the total duration once finished. Tool cards are intentionally not
     rendered in the bubble anymore. */
  .run-line {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 3px 0 9px;
    font-size: 12.5px;
    color: var(--color-muted);
    min-height: 20px;
  }
  .run-prompt {
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--color-primary);
    letter-spacing: -0.5px;
  }
  .run-state { color: var(--color-muted); }
  .run-activity {
    color: var(--color-ink);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 70%;
  }
  .run-cursor {
    width: 6px;
    height: 13px;
    border-radius: 1px;
    background: var(--color-primary);
    animation: run-blink 1.1s steps(2, end) infinite;
  }
  @keyframes run-blink { 50% { opacity: 0; } }
  button.run-line {
    width: 100%;
    border: none;
    background: none;
    text-align: left;
    cursor: pointer;
    font-family: inherit;
    border-radius: 8px;
    padding-left: 4px;
    padding-right: 4px;
  }
  button.run-line:hover { background: var(--color-surface-soft); }
  button.run-line.static { cursor: default; }
  button.run-line.static:hover { background: none; }
  .run-chevron {
    margin-left: 2px;
    font-size: 9px;
    color: var(--color-muted);
    flex: none;
  }
  .run-details {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin: 2px 0 10px;
    padding: 8px 10px;
    border-left: 1px solid var(--color-hairline);
    max-height: 42vh;
    overflow-y: auto;
  }
  .run-details .event-item { margin: 0; }
  .intermediate-text {
    font-size: 11.5px;
    line-height: 1.65;
    color: var(--color-dim);
    padding: 2px 2px 2px 10px;
    border-left: 1px dashed var(--color-hairline-strong);
  }
  .intermediate-text :global(p) { margin: 2px 0; }
  .run-line.done { color: var(--color-muted); font-size: 11.5px; }
  .run-check { color: var(--color-primary); font-size: 11px; }

  .exit-reason-banner {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    margin-top: 10px;
    padding: 0;
    border: none;
    background: none;
    font-size: 11px;
    color: var(--color-gold);
    opacity: 0.85;
    line-height: 1.5;
  }
  .exit-reason-banner::before {
    content: "";
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: currentColor;
    flex: none;
  }
  .bubble-footer {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 10px;
    padding-top: 0;
    border-top: none;
    font-size: 10.5px;
    color: var(--color-muted);
    flex-wrap: wrap;
  }
  .bubble-footer.user-footer {
    border-top: none;
    justify-content: flex-end;
    margin-top: 6px;
    padding-top: 0;
    opacity: 0.75;
  }
  .bubble-footer.running {
    border-top-color: var(--color-gold-line);
  }
  .footer-sep {
    width: 1px;
    height: 10px;
    background: var(--color-hairline);
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
    color: var(--color-dim);
  }
  .footer-exit {
    font-family: var(--font-mono);
    font-size: 10px;
    letter-spacing: 0.02em;
    color: var(--color-dim);
    white-space: nowrap;
  }
  .footer-feedback {
    display: inline-flex;
    gap: 2px;
  }
  .fb-btn {
    border: none;
    background: transparent;
    color: var(--color-muted);
    cursor: pointer;
    font-size: 9px;
    line-height: 1;
    padding: 2px 4px;
    border-radius: 8px;
  }
  .fb-btn:hover:not(:disabled) {
    color: var(--color-body);
    background: var(--color-surface-soft);
  }
  .fb-btn.on {
    color: var(--color-status-warn);
  }
  .fb-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .footer-feedback-done {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: var(--color-success);
  }
  .feedback-note {
    margin: 6px 0 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .feedback-input {
    font-family: inherit;
    font-size: 12px;
    color: var(--color-body);
    background: var(--color-surface-soft);
    border: 1px solid var(--color-hairline);
    border-radius: 6px;
    padding: 6px 8px;
    resize: vertical;
  }
  .feedback-actions {
    display: flex;
    gap: 8px;
  }
  .fb-send,
  .fb-cancel {
    font-family: inherit;
    font-size: 12px;
    padding: 4px 10px;
    border-radius: 6px;
    cursor: pointer;
    border: 1px solid var(--color-hairline);
    background: transparent;
    color: var(--color-body);
  }
  .fb-send:hover {
    border-color: var(--color-primary);
    color: var(--color-primary);
  }
  .feedback-error {
    margin-top: 4px;
    font-size: 12px;
    color: var(--color-error);
  }
  .footer-quality {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    white-space: nowrap;
  }
  .footer-quality .quality-dot {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: currentColor;
    flex: 0 0 auto;
  }
  .footer-quality.ok {
    color: var(--color-success);
  }
  .footer-quality.warn {
    color: var(--color-status-warn);
  }
  .footer-quality.muted {
    color: var(--color-muted);
  }
  .footer-exit.warn {
    color: var(--color-status-warn);
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
