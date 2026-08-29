<script lang="ts">
  import { chat } from "../stores/chat";
  import { sessions } from "../stores/sessions";
  import { compressSession } from "../api/sessions";
  import { invoke } from "@tauri-apps/api/core";
  import { open } from "@tauri-apps/plugin-dialog";
  import CommandPalette from "./CommandPalette.svelte";
  import { t, localT, locale } from "../i18n";

let {
  disabled = $bindable(false),
} = $props();

let inputText = $state("");
let showCommands = $state(false);
let attachMenuOpen = $state(false);
let textareaEl: HTMLTextAreaElement | undefined = $state();
  let stuckCount = $state(0);
  let stuckTimer: ReturnType<typeof setTimeout> | null = null;

  function handleInput() {
    if (textareaEl) {
      textareaEl.style.height = "auto";
      textareaEl.style.height = Math.min(textareaEl.scrollHeight, 160) + "px";
    }
    if (inputText.startsWith("/") && inputText.length > 1) {
      showCommands = true;
    } else {
      showCommands = false;
    }
  }

  async function send() {
    const text = inputText.trim();
    const hasAttachments = $chat.attachments.length > 0;
    if (!text && !hasAttachments) return;

    if (text.startsWith("/")) {
      handleCommand(text);
      inputText = "";
      return;
    }

    inputText = "";
    if (textareaEl) textareaEl.style.height = "auto";

    // If agent is still running, inject message without interrupting.
    // The message appears as an intervention card inside the agent bubble
    // when the agent loop picks it up at the next LLM prefill turn.
    if ($chat.isProcessing) {
      const sid = $sessions.currentId;
      if (sid) {
        await invoke("inject_message", { sessionId: sid, text });
      }
      return;
    }

    const sid = $sessions.currentId;
    await chat.sendMessage(text);
    if (sid) sessions.bumpMessageCount(sid, 1);
  }

  async function handleCommand(cmd: string) {
    showCommands = false;
    const parts = cmd.trim().split(/\s+/);
    const base = parts[0].toLowerCase();
    const arg = parts.slice(1).join(" ");

    switch (base) {
      case "/clear": {
        chat.clearMessages();
        const cid = $sessions.currentId;
        if (cid) invoke("clear_session_messages", { sessionId: cid }).catch(() => {});
        break;
      }
      case "/new":
        sessions.create();
        chat.clearMessages();
        break;
      case "/help":
        chat.addUserMessage(cmd);
        chat.startAssistantMessage();
        chat.appendLocalText(
          localT("help.title") + "\n\n" +
          "- `/help` — " + localT("help.item.help") + "\n" +
          "- `/clear` — " + localT("help.clear") + "\n" +
          "- `/new` — " + localT("help.new") + "\n" +
          "- `/model [name]` — " + localT("help.model") + "\n" +
          "- `/sessions` — " + localT("help.sessions") + "\n" +
          "- `/export` — " + localT("help.export") + "\n" +
          "- `/shortcut` — " + localT("cmd.shortcut.desc")
        );
        chat.finalizeAssistantMessage();
        break;
      case "/model": {
        chat.openModelSwitcher();
        break;
      }
      case "/sessions":
        chat.addUserMessage(cmd);
        chat.startAssistantMessage();
        chat.appendLocalText(localT("compact.sessionCount") + `: ${$sessions.sessions.length}. ` + localT("compact.activeSession") + `: ${$sessions.currentId || localT("compact.none")}.`);
        chat.finalizeAssistantMessage();
        break;
      case "/export": {
        chat.addUserMessage(cmd);
        chat.startAssistantMessage();
        const exportData = JSON.stringify($chat.messages, null, 2);
        chat.appendLocalText("```json\n" + exportData + "\n```");
        chat.finalizeAssistantMessage();
        break;
      }
      case "/compact": {
        const cid = $sessions.currentId;
        if (!cid) break;
        try {
          const result = await compressSession(cid);
          chat.addUserMessage(cmd);
          chat.startAssistantMessage();
          chat.appendLocalText(
            "⚡ " + localT("compact.title") + "\n\n" +
            localT("compact.before") + `: ${result.before_tokens.toLocaleString()} tokens · ` + localT("compact.after") + `: ${result.after_tokens.toLocaleString()} tokens\n` +
            localT("compact.saved") + `: ${result.saved_tokens.toLocaleString()} tokens (${result.saved_pct}%)\n` +
            localT("compact.strategy") + `: ${result.strategy}`
          );
          chat.finalizeAssistantMessage();
          await sessions.load();
        } catch (e: any) {
          chat.addUserMessage(cmd);
          chat.startAssistantMessage();
          chat.appendLocalText(`❌ ${localT("compact.failed")}: ${e?.message || e}`);
          chat.finalizeAssistantMessage();
        }
        break;
      }
      case "/shortcut":
      case "/shortcuts":
        chat.addUserMessage(cmd);
        chat.startAssistantMessage();
        chat.appendLocalText(
          "⌨️ " + localT("shortcuts.title") + "\n\n" +
          "**" + localT("shortcuts.global") + "**\n" +
          "`⌘N` " + localT("shortcuts.new") + "\n" +
          "`⌘[` `⌘]` " + localT("shortcuts.prevNext") + "\n" +
          "`⌘⇧S` " + localT("shortcuts.toggleSidebar") + "\n" +
          "`⌘⇧E` " + localT("shortcuts.toggleSidePanel") + "\n" +
          "`⌘/` " + localT("shortcuts.showPanel") + "\n" +
          "`⌘⇧D` " + localT("shortcuts.deleteSession") + "\n\n" +
          "**" + localT("shortcuts.sidePanel") + "**\n" +
          "`←` `→` " + localT("shortcuts.switchTabs") + "\n" +
          "`Esc` " + localT("shortcuts.closePanel") + "\n\n" +
          "**" + localT("shortcuts.messages") + "**\n" +
          "`C` " + localT("shortcuts.copyLast") + "\n" +
          "`↑` " + localT("shortcuts.focusSidebar") + "\n\n" +
          "**" + localT("shortcuts.input") + "**\n" +
          "`Enter` " + localT("shortcuts.sendMsg") + "\n" +
          "`⇧Enter` " + localT("shortcuts.newline") + "\n" +
          "`Esc` " + localT("shortcuts.cancelTask") + " (" + localT("shortcuts.forceReset") + ")\n" +
          "`/xxx` " + localT("shortcuts.command") + "\n\n" +
          "**" + localT("shortcuts.dialog") + "**\n" +
          "`Esc` " + localT("shortcuts.dialogClose") + "\n" +
          "`Enter` " + localT("shortcuts.dialogConfirm") + "\n" +
          "`Tab` `⇧Tab` " + localT("shortcuts.dialogFocus")
        );
        chat.finalizeAssistantMessage();
        break;
      case "/resume": {
        chat.resume();
        break;
      }
    }
  }

  function onCommandSelect(cmd: string) {
    inputText = cmd + " ";
    showCommands = false;
    if (textareaEl) textareaEl.focus();
  }

  async function pickFile() {
    try {
      const selected = await open({
        multiple: false,
        filters: [{
          name: "Documents",
          extensions: ["pdf", "docx", "pptx", "xlsx", "xls", "doc", "txt", "md", "csv", "json", "yaml", "yml", "toml", "xml", "html", "rtf"],
        }],
      });
      if (selected && typeof selected === "string") {
        const name = selected.split("/").pop() || selected;
        const ext = name.split(".").pop()?.toLowerCase() || "";
        chat.attachFile({ id: `${Date.now()}-f`, path: selected, name, type: "file" });
      }
    } catch (_) {}
  }

  async function pickImage() {
    try {
      const selected = await open({
        multiple: false,
        filters: [{
          name: "Images",
          extensions: ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp"],
        }],
      });
      if (selected && typeof selected === "string") {
        const name = selected.split("/").pop() || selected;
        chat.attachFile({ id: `${Date.now()}-i`, path: selected, name, type: "image" });
      }
    } catch (_) {}
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
    if (e.key === "Escape") {
      if ($chat.isProcessing) {
        stuckCount++;
        if (stuckCount >= 3) {
          stuckCount = 0;
          chat.forceReset();
          if (stuckTimer) { clearTimeout(stuckTimer); stuckTimer = null; }
          return;
        }
        if (!stuckTimer) {
          stuckTimer = setTimeout(() => { stuckCount = 0; stuckTimer = null; }, 4000);
        }
        chat.cancelCurrent();
      } else {
        showCommands = false;
      }
    }
  }

  // Reset stuck count when processing ends normally
  $effect(() => {
    if (!$chat.isProcessing) {
      stuckCount = 0;
      if (stuckTimer) { clearTimeout(stuckTimer); stuckTimer = null; }
    }
  });

</script>

<div class="input-wrapper">
  <CommandPalette
    bind:show={showCommands}
    filter={inputText.slice(1)}
    onSelect={onCommandSelect}
  />

  {#if $chat.attachments.length > 0}
    <div class="attachments-bar">
      {#each $chat.attachments as a (a.id)}
        <div class="attachment-chip" class:image={a.type === "image"}>
          {#if a.type === "image"}
            <svg width="14" height="14" viewBox="0 0 18 18" fill="none">
              <rect x="2" y="4" width="14" height="12" rx="2" stroke="currentColor" stroke-width="1.3"/>
              <circle cx="6" cy="8" r="1.5" fill="currentColor"/>
              <path d="M2 14l4-5 3 3 3-4 4 6H2z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/>
            </svg>
          {:else}
            <svg width="14" height="14" viewBox="0 0 18 18" fill="none">
              <path d="M5 2h6l4 4v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2z" stroke="currentColor" stroke-width="1.3"/>
              <path d="M11 2v4h4" stroke="currentColor" stroke-width="1.3"/>
              <line x1="6" y1="11" x2="12" y2="11" stroke="currentColor" stroke-width="1.2"/>
              <line x1="6" y1="14" x2="10" y2="14" stroke="currentColor" stroke-width="1.2"/>
            </svg>
          {/if}
          <span class="chip-name">{a.name}</span>
          <button
            class="chip-remove"
            onclick={() => chat.removeAttachment(a.id)}
            aria-label={$t("chat.removeAttachment")}
          >
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
              <path d="M2 2l6 6M8 2l-6 6" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>
            </svg>
          </button>
        </div>
      {/each}
    </div>
  {/if}

  <div class="composer-box">
    <textarea
      bind:this={textareaEl}
      bind:value={inputText}
      oninput={handleInput}
      onkeydown={handleKeydown}
      placeholder={disabled ? $t("chat.placeholder.processing") : $t("chat.placeholder")}
      rows="1"
      disabled={disabled}
    ></textarea>
    <div class="composer-row">
      <div class="attach-wrap">
        <button
          class="attach-btn"
          class:open={attachMenuOpen}
          onclick={() => attachMenuOpen = !attachMenuOpen}
          aria-label={$t("chat.attachFile")}
          title={$t("chat.attachFile")}
        >{$t("chat.attach")}</button>
        {#if attachMenuOpen}
          <div class="attach-menu">
            <button class="attach-menu-item" onclick={() => { attachMenuOpen = false; pickFile(); }}>
              <svg class="am-ic" width="14" height="14" viewBox="0 0 18 18" fill="none" aria-hidden="true">
                <path d="M5 2h6l4 4v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2z" stroke="currentColor" stroke-width="1.3"/>
                <path d="M11 2v4h4" stroke="currentColor" stroke-width="1.3"/>
              </svg>
              {$t("chat.attachMenuFile")}
            </button>
            <button class="attach-menu-item" onclick={() => { attachMenuOpen = false; pickImage(); }}>
              <svg class="am-ic" width="14" height="14" viewBox="0 0 18 18" fill="none" aria-hidden="true">
                <rect x="2" y="4" width="14" height="12" rx="2" stroke="currentColor" stroke-width="1.3"/>
                <circle cx="6" cy="8" r="1.5" fill="currentColor"/>
                <path d="M2 14l4-5 3 3 3-4 4 6H2z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/>
              </svg>
              {$t("chat.attachMenuImage")}
            </button>
          </div>
        {/if}
      </div>
      <span class="composer-hint"><kbd>⌘</kbd> {$t("input.send")} · <kbd>⇧⏎</kbd> {$t("input.newline")}</span>
      <span class="composer-spacer"></span>
      {#if $chat.isProcessing}
        <button
          class="seal-btn stop-btn"
          class:en-seal={$locale === "en"}
          data-busy="1"
          onclick={() => chat.cancelCurrent()}
          aria-label={$t("chat.stop")}
          title={$t("chat.stop") + " (Esc)"}
        >{$t("chat.stopSeal")}</button>
      {:else}
        <button
          class="seal-btn send-btn"
          class:en-seal={$locale === "en"}
          onclick={send}
          aria-label={$t("chat.send")}
          disabled={disabled || (!inputText.trim() && !$chat.attachments.length)}
        >{$t("chat.sendSeal")}</button>
      {/if}
    </div>
  </div>
</div>

<style>
  .input-wrapper {
    position: relative;
    /* 与 App.svelte 的 .messages-list 同步: 跟随窗口伸展, 1200px 封顶 */
    max-width: min(100%, 1200px);
    margin: 0 auto;
    width: 100%;
    padding: 8px 24px 16px;
  }

  .attachments-bar {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 10px 16px 0;
  }

  .attachment-chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 4px 6px 4px 8px;
    border-radius: 6px;
    background: var(--color-surface-soft);
    border: 1px solid var(--color-hairline);
    font-size: 12px;
    color: var(--color-ink);
    max-width: 200px;
  }

  .attachment-chip.image {
    background: color-mix(in srgb, var(--color-accent) 10%, transparent);
    border-color: color-mix(in srgb, var(--color-accent) 24%, transparent);
  }

  .chip-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
  }

  .chip-remove {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    border: none;
    background: transparent;
    color: var(--color-muted);
    cursor: pointer;
    flex-shrink: 0;
    transition: background 0.1s, color 0.1s;
  }

  .chip-remove:hover {
    background: var(--color-hairline);
    color: var(--color-error);
  }

  /* ── Composer: 题字落款 (对齐原型 v2) ── */
  .composer-box {
    position: relative;
    border: 1px solid var(--color-hairline);
    border-radius: 4px;
    background: var(--color-surface-soft);
    box-shadow: var(--glaze-shadow, none);
    transition: border-color 0.4s var(--ease-soak, ease), box-shadow 0.4s var(--ease-soak, ease);
  }
  .composer-box:focus-within {
    border-color: var(--color-primary);
    box-shadow: var(--glaze-shadow, none), 0 0 0 3px var(--color-primary-muted, rgba(147, 195, 214, 0.07));
  }

  textarea {
    flex: 1;
    width: 100%;
    background: none;
    border: none;
    border-radius: 4px;
    padding: 12px 14px 4px;
    color: var(--color-ink);
    font-family: var(--font-sans);
    font-size: 14px;
    line-height: 1.5;
    resize: none;
    min-height: 42px;
    max-height: 160px;
    outline: none;
    min-width: 0;
  }

  textarea::placeholder {
    color: var(--color-muted);
  }

  textarea:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .composer-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 4px 10px 10px;
  }
  .composer-spacer {
    flex: 1;
  }

  /* 附 件: 文字铭文钮 + 迷你菜单 */
  .attach-wrap {
    position: relative;
    flex: none;
    display: flex;
    align-items: center;
  }
  .attach-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 2px 10px;
    border-radius: 3px;
    border: 1px solid var(--color-hairline);
    background: none;
    color: var(--color-primary);
    font-family: var(--font-serif);
    font-size: 12px;
    letter-spacing: 0.2em;
    cursor: pointer;
    transition: background 0.3s var(--ease-soak, ease), color 0.3s var(--ease-soak, ease), border-color 0.3s var(--ease-soak, ease);
    white-space: nowrap;
    position: relative;
    overflow: hidden;
  }
  .attach-btn::after {
    content: "";
    position: absolute;
    inset: 0;
    transform: translateX(-110%);
    background: linear-gradient(100deg, transparent, rgba(147, 195, 214, 0.18), transparent);
    transition: transform 0.7s var(--ease-soak, ease);
  }
  .attach-btn:hover::after {
    transform: translateX(110%);
  }
  .attach-btn:hover,
  .attach-btn.open {
    background: var(--color-primary-muted);
    border-color: var(--color-primary);
    color: var(--color-primary-hover);
  }
  .attach-menu {
    position: absolute;
    bottom: calc(100% + 6px);
    left: 0;
    min-width: 120px;
    background: var(--color-surface-elevated);
    border: 1px solid var(--color-hairline);
    border-radius: 3px;
    box-shadow: var(--glaze-shadow, 0 8px 24px rgba(0,0,0,.3));
    padding: 4px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    z-index: 30;
    animation: soak-in 0.3s var(--ease-soak, ease) both;
  }
  .attach-menu-item {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 7px 10px;
    border: none;
    background: none;
    color: var(--color-body);
    font-family: var(--font-serif);
    font-size: 12px;
    letter-spacing: 0.1em;
    cursor: pointer;
    border-radius: 2px;
    text-align: left;
    transition: background 0.25s var(--ease-soak, ease), color 0.25s var(--ease-soak, ease);
  }
  .attach-menu-item:hover {
    background: var(--color-primary-muted);
    color: var(--color-primary);
  }
  .am-ic {
    width: 18px;
    height: 18px;
    display: inline-grid;
    place-items: center;
    border: 1px solid var(--color-hairline);
    border-radius: 2px;
    font-size: 10px;
    color: var(--color-primary);
    flex: none;
  }

  /* 快捷键提示 (与原型一致: 发送 · 换行) */
  .composer-hint {
    font-family: var(--font-serif);
    font-size: 11px;
    letter-spacing: 0.15em;
    color: var(--color-dim);
    user-select: none;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    white-space: nowrap;
  }
  .composer-hint kbd {
    font-family: var(--font-mono);
    font-size: 10px;
    border: 1px solid var(--color-hairline);
    border-radius: 2px;
    padding: 0 4px;
    color: var(--color-muted);
  }

  /* 钤印发送钮: 天青方印「言」 */
  .seal-btn {
    width: 34px;
    height: 34px;
    font-size: 15px;
    flex-shrink: 0;
  }
  /* English labels ("Send"/"Stop") need a wider seal than the single
     Chinese glyphs — auto width with padding keeps the square for zh. */
  .seal-btn.en-seal {
    width: auto;
    min-width: 34px;
    padding: 0 10px;
    font-size: 12px;
    letter-spacing: 0.02em;
  }
  .seal-btn:disabled {
    background: var(--color-surface-elevated);
    color: var(--color-dim);
    box-shadow: none;
    cursor: not-allowed;
  }
  .seal-btn:disabled:hover::after {
    transform: translateX(-120%);
  }
  .seal-btn.stop-btn {
    background: var(--color-surface-elevated);
    color: var(--color-error);
    box-shadow: 0 0 0 1px var(--color-error);
    animation: stop-pulse 1.4s ease-in-out infinite;
  }
  @keyframes stop-pulse {
    0%, 100% { box-shadow: 0 0 0 1px var(--color-error), 0 0 0 0 rgba(192, 90, 62, 0.4); }
    50%      { box-shadow: 0 0 0 1px var(--color-error), 0 0 0 6px rgba(192, 90, 62, 0); }
  }
</style>
