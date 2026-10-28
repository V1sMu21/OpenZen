<script lang="ts">
  import { askConfirm } from "../stores/confirm";
  import { sessions } from "../stores/sessions";
  import { projects } from "../stores/projects";
  import { runningSessionIds, waitingSessionIds } from "../stores/running";
  import type { SessionInfo } from "../api/sessions";
  import { t, locale, localT } from "../i18n";

  let {
    items = $bindable([] as SessionInfo[]),
    currentId = $bindable<string | null>(null),
    onSelectSession = $bindable<(id: string) => void>((_id: string) => {}),
    onEscape = $bindable<(() => void) | null>(null),
    indentLevel = 0,
  } = $props();

  let focusedIndex = $state(-1);
  // P2-22: inline rename (double-click the name). Deleting a session is
  // destructive and was one click with no confirmation anywhere.
  let renamingId = $state<string | null>(null);
  let renameDraft = $state("");

  async function confirmDelete(name: string): Promise<boolean> {
    const label = name || localT("session.defaultName", "session");
    const msg = localT("session.deleteConfirm", "Delete this session?").replace(
      "{name}",
      label,
    );
    return askConfirm(msg, { danger: true });
  }

  function startRename(e: Event, session: SessionInfo) {
    e.stopPropagation();
    renamingId = session.id;
    renameDraft = session.name || "";
  }

  async function commitRename(e: KeyboardEvent | FocusEvent, session: SessionInfo) {
    if (renamingId !== session.id) return;
    const name = renameDraft.trim();
    renamingId = null;
    if (!name || name === session.name) return;
    try {
      await sessions.rename(session.id, name);
      const target = items.find((i) => i.id === session.id);
      if (target) target.name = name;
      items = [...items];
    } catch (err) {
      console.error("[SessionList] rename failed:", err);
    }
  }

  function onRenameKey(e: KeyboardEvent, session: SessionInfo) {
    if (e.key === "Enter") {
      e.preventDefault();
      void commitRename(e, session);
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      renamingId = null;
    } else {
      // Keep list-level navigation from hijacking typing.
      e.stopPropagation();
    }
  }

  function formatDate(iso: string): string {
    try {
      // App-locale aware (P2-24); the manual YYYY-MM-DD HH:mm pattern
      // ignored the locale entirely.
      return new Intl.DateTimeFormat($locale, {
        year: "numeric",
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
      }).format(new Date(iso));
    } catch {
      return "";
    }
  }

  /** One-line date for the list: time today, M/D otherwise. */
  function shortDate(iso: string): string {
    try {
      const d = new Date(iso);
      if (Number.isNaN(d.getTime())) return "";
      const now = new Date();
      if (d.toDateString() === now.toDateString()) {
        return new Intl.DateTimeFormat($locale, { hour: "2-digit", minute: "2-digit" }).format(d);
      }
      return new Intl.DateTimeFormat($locale, { month: "numeric", day: "numeric" }).format(d);
    } catch {
      return "";
    }
  }

  async function handleDelete(e: Event, id: string) {
    e.stopPropagation();
    const labeled = items.find((item) => item.id === id);
    if (!(await confirmDelete(labeled?.name ?? ""))) {
      return;
    }
    // Get project_id before removing from local list
    const target = items.find(item => item.id === id);
    const projectId = target?.project_id;
    console.log("[SessionList] handleDelete: id=", id, "projectId=", projectId);
    // Delete from backend + flat sessions store
    await sessions.remove(id);
    // Also update projects store if session belongs to a project
    if (projectId) {
      projects.removeSessionFromProject(id, projectId);
      console.log("[SessionList] removed from project store:", projectId);
    }
    // Remove from local list so UI updates immediately
    items = items.filter(item => item.id !== id);
    if (focusedIndex >= items.length) {
      focusedIndex = Math.max(0, items.length - 1);
    }
  }

  async function handleKeydown(e: KeyboardEvent) {
    if (items.length === 0) return;

    if ((e.metaKey || e.ctrlKey) && e.key === "f") {
      e.preventDefault();
      return;
    }

    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        focusedIndex = Math.min(focusedIndex + 1, items.length - 1);
        break;
      case "ArrowUp":
        e.preventDefault();
        focusedIndex = Math.max(focusedIndex - 1, 0);
        break;
      case "Enter":
        e.preventDefault();
        if (focusedIndex >= 0 && focusedIndex < items.length) {
          onSelectSession(items[focusedIndex].id);
        }
        break;
      case "Delete":
      case "Backspace":
        e.preventDefault();
        if (focusedIndex >= 0 && focusedIndex < items.length) {
          const target = items[focusedIndex];
          if (await confirmDelete(target.name ?? "")) {
            void handleDelete(new Event("keyboard"), target.id);
          }
        }
        break;
      case "Escape":
        e.preventDefault();
        focusedIndex = -1;
        onEscape?.();
        break;
    }
  }

  $effect(() => {
    const len = items.length;
    if (focusedIndex >= len && len > 0) {
      focusedIndex = len - 1;
    }
  });
</script>

<div
  class="session-list"
  class:indented={indentLevel > 0}
  style="--indent: {indentLevel * 12}px"
  onkeydown={handleKeydown}
  tabindex="0"
  role="listbox"
  aria-label="{$t('session.ariaLabel')}"
>
  {#each items as session, idx (session.id)}
    <div
      class="session-item"
      class:active={session.id === currentId}
      class:focused={idx === focusedIndex}
      role="option"
      aria-selected={session.id === currentId}
      aria-label={$waitingSessionIds.has(session.id) ? $t("session.waiting") : $runningSessionIds.has(session.id) ? $t("session.running") : undefined}
      tabindex="-1"
      onclick={() => { onSelectSession(session.id); focusedIndex = idx; }}
    >
      {#if $waitingSessionIds.has(session.id)}
        <span class="waiting-dot" aria-hidden="true" title={$t("session.waiting")}></span>
      {:else if $runningSessionIds.has(session.id)}
        <span class="running-dot" aria-hidden="true"></span>
      {/if}
      <div class="session-info">
        {#if renamingId === session.id}
          <input
            class="session-rename-input"
            bind:value={renameDraft}
            onkeydown={(e) => onRenameKey(e, session)}
            onblur={(e) => void commitRename(e, session)}
            onfocus={(e) => e.currentTarget.select()}
            aria-label={$t("session.renameHint")}
          />
        {:else}
          <div
            class="session-name"
            role="button"
            tabindex="-1"
            ondblclick={(e) => startRename(e, session)}
            onkeydown={(e) => {
              // Keyboard path for rename: F2 (the list itself owns
              // arrow/Enter/Delete handling).
              if (e.key === "F2") {
                e.preventDefault();
                e.stopPropagation();
                startRename(e, session);
              } else {
                e.stopPropagation();
              }
            }}
            title={`${session.name || $t("session.defaultName")}\n${session.message_count} ${$t("session.msgs")} · ${formatDate(session.created_at)}`}
          >{session.name || $t("session.defaultName")}</div>
        {/if}
      </div>
      {#if renamingId !== session.id}
        <span class="session-time" aria-hidden="true">{shortDate(session.created_at)}</span>
      {/if}
      <button
        class="delete-btn"
        onclick={(e) => handleDelete(e, session.id)}
        aria-label={$t("session.delete")}
      >
        <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
          <path d="M4 4l6 6M10 4l-6 6" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
        </svg>
      </button>
    </div>
  {/each}

  {#if items.length === 0 && indentLevel === 0}
    <div class="empty-state">{$t("session.emptyState")}</div>
  {/if}
</div>

<style>
  .session-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: 2px 4px;
  }

  .session-list.indented {
    padding: 1px 0 4px;
  }

  .session-item {
    position: relative;
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 32px;
    padding: 7px 8px 7px calc(10px + var(--indent, 0px));
    border-radius: 10px;
    background: transparent;
    color: var(--color-dim);
    cursor: pointer;
    text-align: left;
    width: 100%;
    transition: background 0.15s var(--ease-soak), color 0.15s var(--ease-soak);
  }

  .session-item:hover {
    background: var(--color-surface-soft);
    color: var(--color-body);
  }

  .session-item.active {
    background: color-mix(in srgb, var(--color-primary) 9%, transparent);
    color: var(--color-ink);
  }

  /* 活动会话: 左缘一道 2px 天青线 (器物刻痕) */
  .session-item.active::before {
    content: "";
    position: absolute;
    left: 0;
    top: 9px;
    bottom: 9px;
    width: 2px;
    border-radius: 2px;
    background: var(--color-primary);
  }

  .session-item.focused:not(.active) {
    outline: 2px solid var(--color-primary);
    outline-offset: -2px;
    background: var(--color-surface-soft);
  }

  /* ── 运行中 / 待确认: 呼吸点 + 扩散环 (transform/opacity only) ── */
  .running-dot,
  .waiting-dot {
    position: relative;
    flex: none;
    width: 6px;
    height: 6px;
    border-radius: 50%;
  }

  .running-dot { background: var(--color-primary); color: var(--color-primary); }
  .waiting-dot { background: var(--color-gold); color: var(--color-gold); }

  .running-dot::after,
  .waiting-dot::after {
    content: "";
    position: absolute;
    inset: -3px;
    border-radius: 50%;
    background: currentColor;
    opacity: 0.4;
    animation: dot-halo 1.8s ease-out infinite;
  }

  @keyframes dot-halo {
    0% { transform: scale(0.5); opacity: 0.5; }
    70% { transform: scale(1.7); opacity: 0; }
    100% { transform: scale(1.7); opacity: 0; }
  }

  .session-info {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
  }

  .session-name {
    font-family: var(--font-sans);
    font-size: 12.5px;
    letter-spacing: 0;
    font-weight: 500;
    line-height: 1.35;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .session-time {
    flex: none;
    font-family: var(--font-mono);
    font-size: 9.5px;
    color: var(--color-muted);
    opacity: 0.75;
    font-variant-numeric: tabular-nums;
    transition: opacity 0.15s;
  }

  .session-item:hover .session-time {
    opacity: 0;
  }

  .delete-btn {
    flex-shrink: 0;
    width: 24px;
    height: 24px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: none;
    border: none;
    color: var(--color-muted);
    border-radius: 7px;
    cursor: pointer;
    opacity: 0;
    transition: opacity 0.15s, background 0.15s, color 0.15s;
  }

  .session-item:hover .delete-btn,
  .session-item.active .delete-btn {
    opacity: 1;
  }

  .delete-btn:hover {
    background: color-mix(in srgb, var(--color-error) 16%, transparent);
    color: var(--color-error);
  }

  .empty-state {
    padding: 18px 12px;
    text-align: center;
    font-size: 12px;
    color: var(--color-muted);
  }

  .session-rename-input {
    width: 100%;
    font: inherit;
    color: var(--color-ink);
    background: var(--color-surface-soft);
    border: 1px solid var(--color-primary);
    border-radius: 7px;
    padding: 2px 6px;
    outline: none;
  }

  @media (prefers-reduced-motion: reduce) {
    .running-dot::after,
    .waiting-dot::after {
      animation: none;
      opacity: 0.2;
      transform: none;
    }
  }
</style>
