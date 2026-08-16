<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
  import { sessions } from "./lib/stores/sessions";
  import { projects } from "./lib/stores/projects";
  import { chat } from "./lib/stores/chat";
  import { connectSSE, heartbeat } from "./lib/stores/sse";
  import { formatTokenCount, type Message } from "./lib/stores/types";
  import Sidebar from "./lib/components/Sidebar.svelte";
  import ChatMessage from "./lib/components/ChatMessage.svelte";
  import ChatInput from "./lib/components/ChatInput.svelte";
  import AskUserDialog from "./lib/components/AskUserDialog.svelte";
  import ModelSwitcher from "./lib/components/ModelSwitcher.svelte";
  import AuthDialog from "./lib/components/AuthDialog.svelte";
  import ApprovalModal from "./lib/components/ApprovalModal.svelte";
  import TransientsBar from "./lib/components/TransientsBar.svelte";
  import ShortcutsPanel from "./lib/components/ShortcutsPanel.svelte";
  import SidePanel from "./lib/components/SidePanel.svelte";
  import TodoProgress from "./lib/components/TodoProgress.svelte";
  import ReminderCard from "./lib/components/ReminderCard.svelte";
  import SoulCard from "./lib/components/SoulCard.svelte";
  import { initLocale } from "./lib/i18n";
  import { t, locale } from "./lib/i18n";
  import { sidepanel } from "./lib/stores/sidepanel.svelte";
  import ThemeSwitcher from "./lib/components/ThemeSwitcher.svelte";


  let sidebarOpen = $state(true);
  let disableInput = $state(false);
  let showShortcuts = $state(false);
  let sidebarFocused = $state(false);
  let workingDir = $state("");
  let crystallizationOn = $state(false);
  let fullAccessOn = $state(false);
  let showThinkingTimer = $state(false);
  let messagesEnd: HTMLDivElement | undefined = $state();
  let isDragOver = $state(false);

  function attachFromPaths(paths: string[]) {
    for (let i = 0; i < paths.length; i++) {
      const name = paths[i].split("/").pop() || paths[i];
      const ext = name.split(".").pop()?.toLowerCase() || "";
      const isImage = ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp"].includes(ext);
      chat.attachFile({
        id: `${Date.now()}-${i}`,
        path: paths[i],
        name,
        type: isImage ? "image" : "file",
      });
    }
  }

  onMount(() => {
    try {
      const wv = getCurrentWebview();
      wv.onDragDropEvent((event) => {
        const e = event.payload;
        if (e.type === "enter" || e.type === "over") {
          isDragOver = true;
        } else if (e.type === "leave") {
          isDragOver = false;
        } else if (e.type === "drop") {
          isDragOver = false;
          if (e.paths.length > 0) {
            attachFromPaths(e.paths);
          }
        }
      });
    } catch (_) {
      // Not in Tauri — fall back to HTML5 drag
      const fallbackDrop = (e: DragEvent) => {
        e.preventDefault();
        const raw = e.dataTransfer?.getData("text/uri-list") || "";
        const paths: string[] = [];
        if (raw) {
          for (const line of raw.split("\n")) {
            const t = line.trim();
            if (!t) continue;
            let p = t.replace(/^file:\/\//, "");
            try { p = decodeURIComponent(p); } catch (_) {}
            paths.push(p);
          }
        } else if (e.dataTransfer?.files) {
          for (let i = 0; i < e.dataTransfer.files.length; i++) {
            paths.push((e.dataTransfer.files[i] as any).path || e.dataTransfer.files[i].name);
          }
        }
        if (paths.length > 0) attachFromPaths(paths);
      };
      document.body.addEventListener("dragover", (e) => { e.preventDefault(); isDragOver = true; });
      document.body.addEventListener("dragleave", (e) => { if (e.target === document.body) isDragOver = false; });
      document.body.addEventListener("drop", fallbackDrop);
    }
  });

  let ctxWin = $derived.by(() => {
    const cfgWin = $chat.modelInfo?.contextWindow ?? 0;
    return cfgWin > 0 ? cfgWin : 200_000;
  });

  let ctxTokens = $derived.by(() => {
    // Find the last message that has contextTokens set.
    // During streaming, the current message has no contextTokens yet,
    // so we use the previous completed one to avoid flickering to 0.
    const msgs = $chat.messages;
    for (let i = msgs.length - 1; i >= 0; i--) {
      const ctx = msgs[i]?.contextTokens;
      if (ctx && ctx > 0) {
        return ctx;
      }
    }
    return 0;
  }) as unknown as number;

  let ctxPct = $derived.by(() => {
    return ctxWin > 0 ? Math.min(100, Math.max(0, (ctxTokens / ctxWin) * 100)) : 0;
  });

  let ctxColor = $derived(
    ctxPct < 70 ? "#0bf4e1" :
    ctxPct < 90 ? "#f3a90c" :
    "#f60934"
  );

  // Module-level stable array for non-live ChatMessage props. Using one
  // shared reference means a streaming update never dirties the
  // `streamingParts` prop of the other N-1 message components.
  const NO_STREAMING_PARTS: import("./lib/stores/parts").UIMessagePart[] = [];

  // The single authority for "which message is live right now".
  // This is the exact four-condition isLive predicate from
  // docs/correct-rendering-spec.md §3.2, evaluated once per state change
  // instead of once per ChatMessage component.
  let liveMessageId = $derived.by(() => {
    if (!$chat.isProcessing) return null;
    const msgs = $chat.messages;
    if (msgs.length === 0) return null;
    const last = msgs[msgs.length - 1];
    return last.role === "assistant" ? last.id : null;
  });

  let regenerableMessageId = $derived.by(() => {
    if ($chat.isProcessing) return null;
    const msgs = $chat.messages;
    if (msgs.length === 0) return null;
    const last = msgs[msgs.length - 1];
    return last.role === "assistant" && !last.streaming ? last.id : null;
  });

  // ── Incremental derived caches (T3.2) ─────────────────────────────
  // Streaming deltas still notify the legacy store once per rAF frame,
  // but the messages array and every historical message object now keep
  // their identity (see chat.ts T3.1). These caches turn that notification
  // into an O(1) identity check instead of re-filtering/re-summing the
  // whole conversation on every frame.

  function visibleMessageFilter(m: Message): boolean {
    if (m.role === "system") return false;
    // Keep streaming messages (bubble needs to render live) and
    // assistant messages with parts (tool cards etc.) even if content is empty.
    if (m.streaming) return true;
    if (m.role === "assistant" && (m.parts?.length || m.streamEvents?.length)) return true;
    const c = (m.content ?? "").trim();
    if (c === "") return false;
    // Filter bare JSON tool-result stubs (e.g. {"status":"written"})
    if (c.startsWith("{") && c.endsWith("}") && c.length < 80) {
      try { const o = JSON.parse(c); if (Object.keys(o).every((k) => typeof o[k] === "string" && o[k].length < 200)) return false; } catch {}
    }
    return true;
  }

  type VisibleMessagesCache = {
    source: Message[];
    last: Message | undefined;
    value: Message[];
  };
  let visibleMessagesCache: VisibleMessagesCache | null = null;
  let visibleMessages = $derived.by(() => {
    const msgs = $chat.messages;
    const last = msgs[msgs.length - 1];
    // Historical messages are immutable once finalized and the array is
    // append-only during streaming, so array identity + last-message
    // identity is a complete cache key for visibility.
    if (visibleMessagesCache && visibleMessagesCache.source === msgs && visibleMessagesCache.last === last) {
      return visibleMessagesCache.value;
    }
    const value = msgs.filter(visibleMessageFilter);
    visibleMessagesCache = { source: msgs, last, value };
    return value;
  });

  function aggregateTokens(messages: readonly Message[]): { in: number; out: number } {
    let inTotal = 0;
    let outTotal = 0;
    for (const m of messages) {
      if (m.tokensIn) inTotal += m.tokensIn;
      if (m.tokensOut) outTotal += m.tokensOut;
    }
    return { in: inTotal, out: outTotal };
  }

  type TokenTotalsCache = {
    source: Message[];
    last: Message | undefined;
    lastTokensIn: number | undefined;
    lastTokensOut: number | undefined;
    value: { in: number; out: number };
  };
  let tokenTotalsCache: TokenTotalsCache | null = null;
  // Token totals only change when a message is appended/replaced or when
  // the last message's token fields are updated by data_context_usage.
  // During ordinary text streaming, neither happens, so we keep the
  // previous sum instead of rescanning the conversation each frame.
  let tokenTotals = $derived.by(() => {
    const msgs = $chat.messages;
    const last = msgs[msgs.length - 1];
    if (
      tokenTotalsCache
      && tokenTotalsCache.source === msgs
      && tokenTotalsCache.last === last
      && tokenTotalsCache.lastTokensIn === last?.tokensIn
      && tokenTotalsCache.lastTokensOut === last?.tokensOut
    ) {
      return tokenTotalsCache.value;
    }
    const value = aggregateTokens(msgs);
    tokenTotalsCache = {
      source: msgs,
      last,
      lastTokensIn: last?.tokensIn,
      lastTokensOut: last?.tokensOut,
      value,
    };
    return value;
  });

  // 标题栏运行状态指示: 待确认(ask_user 阻塞中) > 运行中 > 完成.
  // 信号全部来自现有 chat store, 无需后端改动.
  let runState = $derived.by((): { cls: "running" | "waiting" | "done"; key: string } => {
    if ($chat.pendingAskUser) return { cls: "waiting", key: "status.waiting" };
    if ($chat.isProcessing) return { cls: "running", key: "status.running" };
    return { cls: "done", key: "status.done" };
  });

  // Subscribe to processing state. When ask_user is pending, the agent
  // loop is still running (just blocked on the user reply), so we keep
  // the chat input enabled — only the AskUserDialog is interactive.
  // The `lastIsFinished` check is a safety net: if the Tauri backend
  // aborts the agent JoinHandle, the `done` SSE event is never emitted
  // and `$chat.isProcessing` can stay `true` forever even though the
  // last assistant turn was finalized by the optimistic
  // `cancelCurrent` path. The message carries its own finalised markers
  // (duration / exitReason), so we trust those too.
  $effect(() => {
    const msgs = $chat.messages;
    const last = msgs.length > 0 ? msgs[msgs.length - 1] : null;
    const lastIsFinished = !!last
      && last.role === "assistant"
      && (last.streaming !== true
        || (last.duration != null && last.duration > 0)
        || (last.exitReason != null && last.exitReason.length > 0));
    // Only disable input during ask_user dialog — user can freely
    // send messages even while agent is processing a long task.
    disableInput = $chat.pendingAskUser != null;
  });

  // Auto-scroll to bottom when the messages-scroll's content changes.
  // We use a MutationObserver on the .messages-list (the direct child
  // of the scrollable .messages-scroll) so we react to:
  //   1. New <ChatMessage> nodes being added (user sent a message,
  //      agent response added).
  //   2. Existing <ChatMessage> nodes having children added/removed
  //      (streaming text appending to the live message).
  //   3. Existing <ChatMessage> nodes' text content changing
  //      (e.g. "thinking" → "answer" transitions).
  //
  // The MutationObserver is the most reliable signal here because:
  //   - Svelte 5 `$effect` does NOT track legacy `writable` store
  //     updates through the `$chat.messages` auto-subscribe syntax.
  //   - The `chat.subscribe()` callback fires synchronously on every
  //     store update, but that fires before the DOM is updated, so
  //     the messages-scroll's scrollHeight would be stale.
  //   - A MutationObserver fires AFTER the DOM is mutated, so by the
  //     time our callback runs, scrollHeight is accurate.
  //
  // We also keep a chat.subscribe() listener to trigger on store
  // changes that don't cause DOM mutations (e.g. isProcessing flag
  // flip while the same message is being streamed).
  onMount(() => {
    initLocale();
    invoke<string>("get_working_dir").then(d => workingDir = d).catch(() => {});
    invoke<boolean>("get_crystallization").then(v => crystallizationOn = v).catch(() => {});
    invoke<boolean>("get_full_access").then(v => fullAccessOn = v).catch(() => {});
    let prevMessageCount = 0;
    let prevStreamingTextLen = 0;
    let userScrolledUp = false;
    const SCROLL_THRESHOLD = 80; // px from bottom to consider "at bottom"

    // ── user scroll-intent tracking ──────────────────────────────────
    // When the user manually scrolls away from the bottom (e.g. to
    // examine a card in a previous turn), we stop auto-scrolling so
    // the viewport stays put. Expanding a card should never yank the
    // user away from what they're reading. Auto-scroll resumes when:
    //   - a new message arrives (grew)           → always scroll
    //   - the user scrolls back near the bottom  → intent resets
    const handleScroll = () => {
      const scroller = document.querySelector<HTMLElement>('.messages-scroll');
      if (!scroller) return;
      const dist = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
      userScrolledUp = dist > SCROLL_THRESHOLD;
    };

    // Helper: scroll the .messages-scroll to the bottom.
    // Throttled: streaming deltas trigger scrollToBottom every frame
    // (MutationObserver + store subscription), so a pending flag
    // collapses all calls in one frame into a single scroll.
    let scrollPending = false;
    const scrollToBottom = () => {
      if (scrollPending) return;
      scrollPending = true;
      requestAnimationFrame(() => {
        requestAnimationFrame(() => {
          scrollPending = false;
          const scroller = document.querySelector<HTMLElement>('.messages-scroll');
          if (scroller) {
            scroller.scrollTop = scroller.scrollHeight;
          } else if (messagesEnd) {
            messagesEnd.scrollIntoView({ behavior: "auto", block: "end" });
          }
        });
      });
    };

    // Helper: try to find the .messages-list element. We poll briefly
    // because the empty-chat → messages-list transition happens AFTER
    // the first message arrives, and the MutationObserver needs the
    // element to exist when it's set up.
    const findMessagesList = (): HTMLElement | null => {
      return document.querySelector<HTMLElement>('.messages-list');
    };

    // Set up MutationObserver as soon as .messages-list exists.
    const setupObserver = () => {
      const messagesList = findMessagesList();
      if (!messagesList) {
        // Try again next frame; the empty chat hasn't been replaced
        // by the messages list yet.
        requestAnimationFrame(setupObserver);
        return;
      }

      // Attach scroll listener to the scrollable container so we can
      // detect when the user manually scrolls up (away from bottom).
      //
      // IMPORTANT: the scrollable element is `.messages-scroll` (it owns
      // `overflow-y: auto`). Scroll events do not bubble, so a listener
      // on `.chat-container` NEVER fires: `userScrolledUp` would stay
      // `false` forever and every DOM mutation (including expanding a
      // card in an earlier message) would yank the viewport to bottom.
      const chatContainer = document.querySelector<HTMLElement>('.messages-scroll')
        ?? messagesList.parentElement ?? null;
      if (chatContainer) {
        chatContainer.addEventListener('scroll', handleScroll, { passive: true });
      }

      const observer = new MutationObserver(() => {
        // Only auto-scroll if the user hasn't scrolled up manually.
        // Card expansion inside earlier messages triggers DOM mutations
        // (subtree childList) but should NOT yank the viewport away.
        if (!userScrolledUp) {
          scrollToBottom();
        }
      });
      observer.observe(messagesList, {
        childList: true,    // new <ChatMessage> nodes
        subtree: true,      // children inside ChatMessage (streaming text, etc.)
        characterData: true, // text content changes
      });

      // Initial scroll (loading a session with existing messages).
      userScrolledUp = false;
      scrollToBottom();
    };
    setupObserver();

    // Also subscribe to the chat store for changes that don't cause
    // DOM mutations (e.g. isProcessing toggling, streamingParts
    // updates that are coalesced). The MutationObserver handles the
    // common case, but the store subscription is a safety net.
    const unsub = chat.subscribe((s) => {
      const count = s.messages.length;
      let streamingTextLen = 0;
      if (s.isProcessing) {
        for (const p of s.streamingParts) {
          if ((p.type === "text" || p.type === "reasoning") && typeof (p as { text?: string }).text === "string") {
            streamingTextLen += (p as { text: string }).text.length;
          }
        }
      }
      const grew = count > prevMessageCount;
      const streamed = streamingTextLen > prevStreamingTextLen;

      if (grew) {
        // New message arrived — always scroll to the bottom and
        // reset the user's scroll intent so they see the new msg.
        userScrolledUp = false;
        scrollToBottom();
      } else if (streamed && !userScrolledUp) {
        // Tokens streaming — only scroll if the user is already
        // near the bottom. If they've scrolled up to read a card
        // or a previous message, stay put.
        scrollToBottom();
      }

      prevMessageCount = count;
      prevStreamingTextLen = streamingTextLen;
    });

    return () => {
      unsub();
      // Remove scroll listener
      const chatContainer = document.querySelector<HTMLElement>('.messages-scroll');
      if (chatContainer) {
        chatContainer.removeEventListener('scroll', handleScroll);
      }
    };
  });

  async function handleNewChat(projectId?: string) {
    const result = await sessions.create();
    if (result) {
      handleSelectSession(result.session_id);
    } else {
      chat.clearMessages();
    }
  }

  async function handleSelectSession(id: string) {
    const prevId = $sessions.currentId;
    if (prevId && prevId !== id) {
      chat.saveSessionState(prevId);
    }
    sessions.select(id);
    localStorage.setItem("currentSessionId", id);
    chat.loadSession(id);
    // Update working directory display from session's project
    invoke<string>("get_working_dir_for_session", { sessionId: id })
      .then(d => workingDir = d)
      .catch(() => {});
  }

  function toggleSidebar() {
    sidebarOpen = !sidebarOpen;
  }

  function toggleCrystallization() {
    crystallizationOn = !crystallizationOn;
    invoke("set_crystallization", { enabled: crystallizationOn }).catch(() => {});
  }

  function toggleFullAccess() {
    fullAccessOn = !fullAccessOn;
    invoke("set_full_access", { enabled: fullAccessOn }).catch(() => {});
  }

  function focusSidebar() {
    if (!sidebarOpen) sidebarOpen = true;
    sidebarFocused = true;
    // Focus the session list container so Arrow keys work
    requestAnimationFrame(() => {
      const list = document.querySelector<HTMLElement>('.session-list');
      list?.focus();
    });
  }

  function handleSidebarEscape() {
    sidebarFocused = false;
    // Focus back to ChatInput
    requestAnimationFrame(() => {
      const input = document.querySelector<HTMLTextAreaElement>('.input-area textarea');
      input?.focus();
    });
  }

  function handleGlobalKeydown(e: KeyboardEvent) {
    const mod = e.metaKey || e.ctrlKey;
    // Use document.activeElement, NOT e.target: synthetic/scripted key
    // events (CGEvent Unicode injection, IME, etc.) can carry a wrong
    // target (body/document) while the textarea still holds focus and
    // receives the characters. Basing isInput on e.target makes
    // single-key shortcuts like `r` fire mid-typing (e.g. `/resume`).
    const activeEl = document.activeElement as HTMLElement | null;
    const isInput = !!activeEl && (
      activeEl.tagName === "INPUT"
      || activeEl.tagName === "TEXTAREA"
      || activeEl.isContentEditable
    );

    // ── Global: always available (even when ChatInput focused) ──
    if (mod && e.key === "n") {
      e.preventDefault();
      handleNewChat();
      return;
    }
    if (mod && e.key === "[") {
      e.preventDefault();
      const id = sessions.previous();
      if (id) handleSelectSession(id);
      return;
    }
    if (mod && e.key === "]") {
      e.preventDefault();
      const id = sessions.next();
      if (id) handleSelectSession(id);
      return;
    }
    if (mod && e.shiftKey && e.key === "S") {
      e.preventDefault();
      toggleSidebar();
      return;
    }
    // ── Right Side Panel: ⌘⇧E (mirrors ⌘⇧S) ──
    if (mod && e.shiftKey && e.key === "E") {
      e.preventDefault();
      sidepanel.toggle();
      return;
    }
    if (mod && e.key === "/") {
      e.preventDefault();
      showShortcuts = !showShortcuts;
      return;
    }
    if (mod && e.shiftKey && e.key === "D") {
      e.preventDefault();
      const current = $sessions.currentId;
      if (current) {
        sessions.remove(current);
        const remaining = $sessions.sessions.filter(s => s.id !== current);
        if (remaining.length > 0) {
          handleSelectSession(remaining[remaining.length - 1].id);
        } else {
          handleNewChat();
        }
      }
      return;
    }

    // ── ChatInput NOT focused: single-key shortcuts ──
    if (!isInput && !mod) {
      // Side Panel: Arrow keys for tab switching
      if (sidepanel.visible) {
        if (e.key === "ArrowLeft") {
          e.preventDefault();
          sidepanel.prevTab();
          return;
        }
        if (e.key === "ArrowRight") {
          e.preventDefault();
          sidepanel.nextTab();
          return;
        }
        if (e.key === "Escape") {
          e.preventDefault();
          sidepanel.visible = false;
          requestAnimationFrame(() => {
            const input = document.querySelector<HTMLTextAreaElement>(".input-area textarea");
            input?.focus();
          });
          return;
        }
      }

      if (e.key === "c" || e.key === "C") {
        e.preventDefault();
        const msgs = $chat.messages;
        const last = msgs.length > 0 ? msgs[msgs.length - 1] : null;
        if (last?.role === "assistant") {
          const text = last.parts?.filter(p => p.type === 'text').map(p => p.text).join('') || last.content;
          if (text) {
            navigator.clipboard.writeText(text).catch(() => {});
          }
        }
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        focusSidebar();
        return;
      }
    }

    // ── Sidebar focused → handled by SessionList onkeydown (bubbles) ──
  }

  onMount(async () => {
    // One startup pass for both stores; api/sessions dedupes the
    // overlapping all-sessions fetch so this is projects + sessions,
    // not projects + sessions + sessions (the old Sidebar onMount).
    await Promise.all([sessions.load(), projects.loadAll()]);

    // Dedicated session windows (`session-{id}`) bind to their own session.
    // The label is set by open_session_window in Rust — authoritative, does
    // not depend on localStorage or on window open order (P2-1).
    let windowSessionId: string | null = null;
    try {
      const label = getCurrentWebviewWindow().label;
      if (label.startsWith("session-")) {
        windowSessionId = label.slice("session-".length);
      }
    } catch {
      // Browser (non-Tauri) context — no window label available.
    }

    const savedId = localStorage.getItem("currentSessionId");
    const restoreId = windowSessionId ?? savedId;

    if ($sessions.sessions.length > 0) {
      // If the window-bound/saved session still exists on server, restore it
      const stillExists = restoreId && $sessions.sessions.some((s) => s.id === restoreId);
      if (stillExists && restoreId) {
        handleSelectSession(restoreId);
      } else {
        // Pick the most recent session from what the server has
        const latest = [...$sessions.sessions].sort(
          (a, b) => new Date(b.created_at).getTime() - new Date(a.created_at).getTime(),
        )[0];
        handleSelectSession(latest.id);
      }
    } else {
      // No sessions on server — create a new one AND load it into chat
      const result = await sessions.create("New Chat");
      if (result) {
        handleSelectSession(result.session_id);
      } else {
        chat.clearMessages();
      }
    }

    // Connect real-time event stream (SSE for browser, Tauri event listener for desktop)
    connectSSE();

    // Initialize Side Panel state from Rust backend
    sidepanel.init();
    sidepanel.setupListeners();

  });
</script>

<svelte:window onkeydown={handleGlobalKeydown} />

<div class="app-layout">
  <!-- Celadon glaze atmosphere layers -->
  <div class="glaze-atmosphere"></div>
  <div class="glaze-noise"></div>
  <div class="glaze-crackle"></div>
  <div class="glaze-shimmer"></div>

  <Sidebar bind:sidebarOpen onNewChat={handleNewChat} onSelectSession={handleSelectSession} onSidebarEscape={handleSidebarEscape} />

  <main class="main-area glaze-surface glaze-pool"
  >
    <!-- 器物底款标题栏 -->
    <div class="main-header">
      <button class="header-btn header-btn-left" onclick={toggleSidebar} title="Toggle Sidebar (⌘⇧S)">
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <rect width="18" height="18" x="3" y="3" rx="2"/>
          <path d="M9 3v18"/>
          {#if sidebarOpen}
            <!-- 打开中 → 点击将收起（向左） -->
            <path d="m14 9-3 3 3 3"/>
          {:else}
            <!-- 已收起 → 点击将展开（向右） -->
            <path d="m11 9 3 3-3 3"/>
          {/if}
        </svg>
      </button>
      <span class="seal" title="OpenZen"><img class="seal-icon" src="/cat-icon.png" alt="OpenZen" /></span>
      <span class="title-name">修砚</span>
      <span class="inscription era">丙午 制</span>
      <!-- 运行状态指示: 活跃态(运行中/待确认)带墨滴涟漪动画, 完成态静态圆点 -->
      <span class="run-state {runState.cls}" title={$t(runState.key)}>
        {#if runState.cls === "done"}
          <span class="state-dot"></span>
        {:else}
          <span class="ink-ripple" aria-hidden="true">
            <span class="ink-ring"></span>
            <span class="ink-ring"></span>
            <span class="ink-dot"></span>
          </span>
        {/if}
        <span class="state-label">{$t(runState.key)}</span>
      </span>
      {#if workingDir}
        <span class="title-path" title={workingDir}>{workingDir}</span>
      {/if}
      <span class="header-spacer"></span>
      <!-- 结晶 / 计时开关 (标题栏最右) -->
      <label class="crystallization-switch" title={crystallizationOn ? $t("status.autoCrystallize") + "：开" : $t("status.autoCrystallize") + "：关"}>
        <input type="checkbox" checked={crystallizationOn} onchange={toggleCrystallization} />
        <span class="switch-track"></span>
        <span class="switch-label">{$t("status.autoCrystallize")}</span>
      </label>
      <label class="crystallization-switch" title={showThinkingTimer ? $t("status.timer") + "：" + $t("status.on") : $t("status.timer") + "：" + $t("status.off")}>
        <input type="checkbox" checked={showThinkingTimer} onchange={() => showThinkingTimer = !showThinkingTimer} />
        <span class="switch-track"></span>
        <span class="switch-label">{$t("status.timer")}</span>
      </label>
      <!-- 完全访问: 开启后 agent 执行任务不再请求权限 (免弹窗自动放行) -->
      <label class="crystallization-switch" title={$t("status.fullAccess") + "：" + (fullAccessOn ? $t("status.on") : $t("status.off"))}>
        <input type="checkbox" checked={fullAccessOn} onchange={toggleFullAccess} />
        <span class="switch-track"></span>
        <span class="switch-label">{$t("status.fullAccess")}</span>
      </label>
      <ThemeSwitcher />
      <button class="header-btn header-btn-right" onclick={() => sidepanel.toggle()} title="Toggle Panel (⌘⇧E)">
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <rect width="18" height="18" x="3" y="3" rx="2"/>
          <path d="M15 3v18"/>
          {#if sidepanel.visible}
            <!-- 已打开 → 点击将收起（向右） -->
            <path d="m7 15 3-3-3-3"/>
          {:else}
            <!-- 关闭中 → 点击将展开（向左） -->
            <path d="m10 15-3-3 3-3"/>
          {/if}
        </svg>
      </button>
    </div>

    <!-- Chat messages -->
    <div class="chat-container">
      {#if $chat.messages.length === 0}
        <div class="empty-chat">
          <div class="empty-icon">
            <svg width="40" height="40" viewBox="0 0 40 40" fill="none">
              <rect x="4" y="8" width="32" height="24" rx="4" stroke="currentColor" stroke-width="1.5"/>
              <path d="M4 18h32" stroke="currentColor" stroke-width="1.5"/>
              <circle cx="10" cy="13" r="1.5" fill="currentColor"/>
              <circle cx="14" cy="13" r="1.5" fill="currentColor"/>
              <circle cx="18" cy="13" r="1.5" fill="currentColor"/>
            </svg>
          </div>
          <h2 class="empty-title">OpenZen</h2>
          <p class="empty-subtitle">{$t("empty.subtitle")}</p>
          <div class="empty-hints">
            <span class="hint">{@html $t("chat.placeholder.help", "Type /help for commands")}</span>
            <span class="hint">{@html $t("empty.hint")}</span>
          </div>
        </div>
      {:else}
        <div class="messages-scroll">
          <!-- 卷轴天头: 竖排款识 -->
          <span class="vertical-rl qing head-rl">卷 一 · 修 砚 之 录</span>
          <div class="messages-list">
            {#if $chat.hasMoreMessages}
              <button
                class="load-earlier-btn"
                onclick={() => chat.loadEarlierMessages()}
                disabled={$chat.loadingEarlier === true}
              >
                {#if $chat.loadingEarlier}
                  <span class="load-earlier-dot" aria-hidden="true"></span>
                {/if}
                {$t("message.loadEarlier", "Load earlier messages")}
              </button>
            {/if}
            {#each visibleMessages as msg (msg.id)}
              <ChatMessage
                message={msg}
                showTimer={showThinkingTimer}
                workingDir={workingDir}
                isLive={liveMessageId === msg.id}
                streamingParts={liveMessageId === msg.id ? $chat.streamingParts : NO_STREAMING_PARTS}
                canRegenerate={regenerableMessageId === msg.id}
              />
            {/each}

            <div bind:this={messagesEnd}></div>
          </div>

          {#if $chat.todos.length > 0 || $chat.reminders.length > 0}
            <div class="todo-rail">
              {#if $chat.todos.length > 0}
                <TodoProgress items={$chat.todos} />
              {/if}
              {#if $chat.reminders.length > 0}
                <!-- 定时/心跳任务卡片：位于待办事项卡片下方 -->
                <ReminderCard items={$chat.reminders} />
              {/if}
            </div>
          {/if}
          <!-- 灵魂/记忆状态卡片：自隐藏（未启用/未加载时不占布局），
               ERME 启用时以 sticky 卡片显示在会话区右侧 -->
          <SoulCard />
        </div>
      {/if}

      <!-- Error banner -->
      {#if $chat.error}
        <div class="error-banner">
          <span>{$chat.error}</span>
          <button class="dismiss-btn" onclick={() => chat.setError("")} aria-label="Dismiss">
            <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
              <path d="M3 3l8 8M11 3l-8 8" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
            </svg>
          </button>
        </div>
      {/if}

      <!-- Compression notice banner -->
      {#if $chat.compressionNotice}
        <div class="compression-banner">
          <span>{$chat.compressionNotice}</span>
        </div>
      {/if}
    </div>

      <!-- Input area -->
      <ChatInput disabled={disableInput} />

      {#if $chat.pendingAskUser}
        <AskUserDialog pending={$chat.pendingAskUser} />
      {/if}

      {#if $chat.showModelSwitcher}
        <ModelSwitcher />
      {/if}

      <div class="model-bar">
        {#if $chat.modelInfo}
          <span class="m-tag m-tag-model">
            <span class="m-tag-num">{$chat.modelInfo.model}</span>
            <span class="m-tag-provider-tag">{!$chat.modelInfo.isLocal ? $t("status.cloud") : $t("status.localDeploy")}</span>
          </span>
        {:else}
          <span class="m-tag m-tag-model">
            <span class="m-tag-num">—</span>
          </span>
        {/if}

        {#if $chat.messages.length > 0}
          <span class="m-sep"></span>
          <span class="m-tag m-tag-muted">
            <span class="m-tag-num">{$chat.messages.length}</span>
            <span class="m-tag-label">{$t("status.msgs")}</span>
          </span>
          {#if ctxTokens > 0 || tokenTotals.out > 0}
            <span class="m-tag m-tag-out">
              <span class="m-tag-label">{$t("status.out")}</span>
              <span class="m-tag-num">{formatTokenCount(tokenTotals.out)}</span>
            </span>
            <span class="m-tag m-tag-in">
              <span class="m-tag-label">{$t("status.in")}</span>
              <span class="m-tag-num">{formatTokenCount(ctxTokens)}</span>
            </span>
          {/if}
          <!-- ctx 用量: 输入/输出统计右边, 后端状态左边 -->
          <span class="m-tag m-tag-ctx" title="Context Usage">
            <span class="ctx-bar"><span class="ctx-fill" style="width:{ctxPct}%; background:{ctxColor};"></span></span>
            <span class="ctx-text">{formatTokenCount(ctxTokens)}/{formatTokenCount(ctxWin)}</span>
          </span>
        {/if}

        <span class="m-sep"></span>

        <span class="m-tag m-tag-health" title="Backend heartbeat: {$heartbeat.connected ? 'connected' : 'disconnected'} | Scheduler: {$heartbeat.scheduler ? 'on' : 'off'} | Sessions: {$heartbeat.sessions} | Agents: {$heartbeat.runningAgents}">
          <span class="health-dot" class:online={$heartbeat.connected}></span>
          <span class="m-tag-label">{$t("status.backend")}</span>
        </span>
      </div>

    {#if isDragOver}
      <div class="drop-overlay">
        <div class="drop-zone">
          <svg width="48" height="48" viewBox="0 0 48 48" fill="none">
            <path d="M24 8v24M14 18l10-10 10 10" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"/>
            <path d="M10 34h28" stroke="currentColor" stroke-width="2.5" stroke-linecap="round"/>
          </svg>
          <span>{$t("chat.dragHint")}</span>
        </div>
      </div>
    {/if}
  </main>

  <SidePanel />
</div>

<AuthDialog />
<ApprovalModal />
<ShortcutsPanel bind:show={showShortcuts} />

<TransientsBar parts={$chat.streamingParts.filter(p => p.type === 'data' && p.transient) as import("./lib/stores/parts").DataPart[]} />

<style>
  .app-layout {
    display: flex;
    height: 100vh;
    overflow: hidden;
  }

  .main-area {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-width: 0;
    position: relative;
  }

  .sidebar-toggle {
    display: none;
    position: absolute;
    top: 8px;
    left: 8px;
    z-index: 10;
    width: 36px;
    height: 36px;
    border-radius: 8px;
    background: var(--color-surface-elevated);
    border: 1px solid var(--color-hairline);
    color: var(--color-body);
    cursor: pointer;
    align-items: center;
    justify-content: center;
  }

  .sidebar-toggle:hover {
    background: var(--color-surface-soft);
  }

  .main-header {
    display: flex;
    align-items: center;
    gap: 12px;
    height: 38px;
    padding: 0 8px 0 12px;
    border-bottom: 1px solid var(--border-color, #333);
    flex-shrink: 0;
  }
  .title-name {
    font-family: var(--font-serif);
    font-size: 13px;
    letter-spacing: 0.12em;
    color: var(--color-muted);
    user-select: none;
    white-space: nowrap;
  }
  .header-btn {
    background: none;
    border: none;
    color: var(--text-muted, #888);
    cursor: pointer;
    font-size: 16px;
    padding: 2px 8px;
    border-radius: 4px;
    line-height: 1;
    transition: color 0.15s;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }
  .header-btn:hover {
    color: var(--text-primary, #eee);
    background: var(--bg-hover, #2a2a4e);
  }
  .header-spacer { flex: 1; }

  /* ctx 用量: 状态栏内一行小字 + 青线 */
  .ctx-bar {
    width: 72px;
    height: 3px;
    background: var(--color-hairline);
    border-radius: 2px;
    overflow: hidden;
  }
  .ctx-fill {
    display: block;
    height: 100%;
    border-radius: 2px;
    transition: width 0.4s var(--ease-soak, ease);
  }
  .ctx-text {
    font-variant-numeric: tabular-nums;
  }
  .m-tag-ctx {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--color-muted);
  }

  /* 消息区: 卷轴天头 + 叙事流
     NOTE: 此容器承担纵向滚动（overflow-y: auto），
     .todo-rail 作为其直接子级 sticky top:0，
     才能横跨整个滚动内容固定在可视区右上角。 */
  .messages-scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    align-items: flex-start;
    gap: 24px;
  }
  .head-rl {
    font-size: 12px;
    padding: 8px 4px 8px 8px;
    border-left: 1px solid var(--color-hairline);
    flex: none;
    align-self: stretch;
    margin-top: 4px;
  }

  .chat-container {
    flex: 1;
    overflow: hidden;
    display: flex;
    flex-direction: column;
    padding: 24px 20px 16px;
    gap: 16px;
  }

  .messages-list {
    display: flex;
    flex-direction: column;
    gap: 16px;
    flex: 1;
    max-width: 660px;
    min-width: 0;
  }

  .load-earlier-btn {
    align-self: center;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 6px 14px;
    border: 1px solid var(--color-hairline);
    border-radius: 999px;
    background: var(--bg-tertiary);
    color: var(--text-secondary);
    font-size: 12px;
    cursor: pointer;
    opacity: 0.85;
    transition: opacity 0.15s;
  }
  .load-earlier-btn:hover {
    opacity: 1;
  }
  .load-earlier-btn:disabled {
    cursor: default;
    opacity: 0.55;
  }
  .load-earlier-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--color-primary);
    animation: loadEarlierPulse 1s ease-out infinite;
  }
  @keyframes loadEarlierPulse {
    0% { opacity: 0.25; transform: scale(0.8); }
    50% { opacity: 1; transform: scale(1); }
    100% { opacity: 0.25; transform: scale(0.8); }
  }

  /* 待办侧栏: 固定于会话区右侧空白顶部, 不随消息滚动 */
  .todo-rail {
    flex: none;
    width: 320px;
    position: sticky;
    top: 0;
    align-self: flex-start;
    margin-top: 4px;
  }
  .todo-rail .todo-progress {
    margin: 0;
  }

  .empty-chat {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    padding: 40px;
  }

  .empty-icon {
    color: var(--color-dim);
    margin-bottom: 8px;
  }

  .empty-title {
    font-size: 24px;
    font-weight: 600;
    color: var(--color-ink);
    letter-spacing: -0.3px;
    margin: 0;
  }

  .empty-subtitle {
    font-size: 15px;
    color: var(--color-muted);
    margin: 0;
  }

  .empty-hints {
    display: flex;
    gap: 16px;
    margin-top: 16px;
  }

  .hint {
    font-size: 12px;
    color: var(--color-dim);
  }

  .hint kbd {
    padding: 1px 5px;
    border-radius: 4px;
    background: var(--color-surface-elevated);
    border: 1px solid var(--color-hairline);
    font-family: var(--font-mono);
    font-size: 11px;
  }

  .error-banner {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 16px;
    background: var(--color-error-bg, rgba(220, 38, 38, 0.1));
    border: 1px solid var(--color-error, #dc2626);
    border-radius: 8px;
    color: var(--color-error, #dc2626);
    font-size: 13px;
  }

  .dismiss-btn {
    margin-left: auto;
    background: none;
    border: none;
    color: var(--color-error);
    cursor: pointer;
    padding: 2px;
    opacity: 0.7;
  }

  .dismiss-btn:hover {
    opacity: 1;
  }

  .compression-banner {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 16px;
    background: var(--color-surface-elevated);
    border: 1px solid var(--color-hairline);
    border-radius: 8px;
    color: var(--color-body);
    font-size: 12px;
    animation: fadeIn 0.2s ease-out;
  }

  @keyframes fadeIn {
    from { opacity: 0; transform: translateY(-4px); }
    to { opacity: 1; transform: translateY(0); }
  }

  /* 标题栏工作目录 (原型 v2: 修砚 · 丙午制 之后, mono 小字) */
  .title-path {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--color-dim);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 260px;
    user-select: none;
    flex-shrink: 1;
    min-width: 0;
  }

  .model-bar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 16px;
    border-top: 1px solid var(--color-hairline);
    background: var(--color-canvas);
    font-size: 11px;
    color: var(--color-dim);
    flex-shrink: 0;
    overflow-x: auto;
  }

  .m-tag {
    display: inline-flex;
    align-items: baseline;
    gap: 5px;
    padding: 1px 8px;
    border-radius: 2px;
    border: 1px solid transparent;
    flex-shrink: 0;
    white-space: nowrap;
  }

  /* 运行状态指示 (标题栏, 标题右侧):
     运行中/待确认 = 墨滴涟漪动画(天青/琥珀), 完成 = 静态暗灰圆点.
     model-bar 不再承载状态动画, 只保留数据统计. */
  .run-state {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    flex: none;
    user-select: none;
    --state-color: var(--color-dim);
  }
  .run-state.running {
    --state-color: var(--color-primary);
    color: var(--color-primary);
  }
  .run-state.waiting {
    --state-color: #f59e0b;
    color: #f59e0b;
  }
  .run-state.done {
    color: var(--color-dim);
  }
  .state-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--color-dim);
    flex: none;
  }
  .state-label {
    font-family: var(--font-serif);
    font-size: 10px;
    letter-spacing: 0.15em;
    white-space: nowrap;
  }
  .run-state .ink-ripple {
    position: relative;
    width: 12px;
    height: 12px;
    flex: none;
    pointer-events: none;
    align-self: center;
  }
  .run-state .ink-dot {
    position: absolute;
    inset: 3px;
    border-radius: 50%;
    background: var(--state-color);
    box-shadow: 0 0 8px var(--state-color);
  }
  .run-state .ink-ring {
    position: absolute;
    inset: 0;
    border-radius: 50%;
    border: 1px solid var(--state-color);
    animation: inkRipple 2.4s ease-out infinite;
  }
  .run-state .ink-ring:nth-of-type(2) {
    animation-delay: 1.2s;
  }
  @keyframes inkRipple {
    0% { transform: scale(0.4); opacity: 0.9; }
    100% { transform: scale(2.6); opacity: 0; }
  }

  .m-tag-num {
    font-weight: 500;
    font-size: 11px;
    letter-spacing: 0.02em;
  }

  .m-tag-label {
    font-family: var(--font-serif);
    font-size: 10px;
    letter-spacing: 0.15em;
    opacity: 0.65;
  }

  .m-tag-model {
    color: var(--color-primary);
    border-color: var(--color-primary);
    font-family: var(--font-serif);
    letter-spacing: 0.06em;
    background: var(--color-primary-muted);
  }

  .m-tag-provider-tag {
    font-family: var(--font-mono);
    font-size: 9.5px;
    color: var(--color-dim);
    border-left: 1px solid var(--color-hairline);
    padding-left: 6px;
  }

  .m-tag-muted {
    color: var(--color-muted);
  }

  .m-tag-out {
    color: var(--color-muted);
    font-variant-numeric: tabular-nums;
  }

  .m-tag-in {
    color: var(--color-primary);
    font-variant-numeric: tabular-nums;
  }

  .m-sep {
    width: 1px;
    height: 10px;
    background: var(--color-hairline);
    opacity: 0.6;
    flex-shrink: 0;
  }

  .m-tag-health {
    color: var(--color-dim);
  }

  .health-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--color-error);
    flex-shrink: 0;
    align-self: center;
    transition: background 0.3s;
  }

  .health-dot.online {
    background: var(--color-primary);
    box-shadow: 0 0 4px var(--color-primary);
  }

  .crystallization-switch {
    display: flex;
    align-items: center;
    gap: 5px;
    cursor: pointer;
    user-select: none;
  }
  .crystallization-switch input {
    display: none;
  }
  .switch-label {
    font-family: var(--font-serif);
    font-size: 10px;
    letter-spacing: 0.1em;
    color: var(--color-dim);
  }
  .switch-track {
    position: relative;
    width: 28px;
    height: 14px;
    background: var(--color-hairline);
    border-radius: 2px;
    transition: background 0.3s var(--ease-soak, ease);
  }
  .switch-track::after {
    content: "";
    position: absolute;
    top: 2px;
    left: 2px;
    width: 10px;
    height: 10px;
    background: var(--color-muted);
    border-radius: 1px;
    transition: transform 0.3s var(--ease-soak, ease), background 0.3s;
  }
  .crystallization-switch:has(input:checked) .switch-track {
    background: var(--color-primary-muted);
  }
  .crystallization-switch:has(input:checked) .switch-track::after {
    transform: translateX(14px);
    background: var(--color-primary);
  }

  @media (max-width: 1100px) {
    .todo-rail {
      display: none;
    }
  }

  @media (max-width: 900px) {
    .sidebar-toggle {
      display: flex;
    }
  }

  .drop-overlay {
    position: absolute;
    inset: 0;
    z-index: 100;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(0, 0, 0, 0.45);
    backdrop-filter: blur(4px);
    pointer-events: all;
  }

  .drop-zone {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    padding: 48px 80px;
    border: 2px dashed var(--color-primary);
    border-radius: 16px;
    color: var(--color-primary);
    background: rgba(255, 255, 255, 0.06);
  }

  .drop-zone svg {
    opacity: 0.8;
    animation: drop-bounce 1.2s ease-in-out infinite;
  }

  .drop-zone span {
    font-size: 16px;
    font-weight: 600;
    letter-spacing: 0.02em;
  }

  @keyframes drop-bounce {
    0%, 100% { transform: translateY(0); }
    50% { transform: translateY(-6px); }
  }
</style>
