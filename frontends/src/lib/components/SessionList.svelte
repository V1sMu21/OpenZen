<script lang="ts">
  import { sessions } from "../stores/sessions";
  import { projects } from "../stores/projects";
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

  function confirmDelete(name: string): boolean {
    const label = name || localT("session.defaultName", "session");
    return window.confirm(
      localT("session.deleteConfirm", "Delete this session?").replace("{name}", label),
    );
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

  async function handleDelete(e: Event, id: string) {
    e.stopPropagation();
    const labeled = items.find((item) => item.id === id);
    if (!confirmDelete(labeled?.name ?? "")) {
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

  function handleKeydown(e: KeyboardEvent) {
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
          if (confirmDelete(target.name ?? "")) {
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
      tabindex="-1"
      onclick={() => { onSelectSession(session.id); focusedIndex = idx; }}
    >
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
            title={$t("session.renameHint")}
          >{session.name || $t("session.defaultName")}</div>
        {/if}
        <div class="session-meta">
          <span>{session.message_count} {$t("session.msgs")}</span>
          <span>{formatDate(session.created_at)}</span>
        </div>
      </div>
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
    gap: 2px;
    padding: 4px 8px;
  }

  .session-list.indented {
    padding: 2px 4px 2px 0;
  }

  .session-item {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 6px 8px 6px calc(12px + var(--indent, 0px));
    border-radius: 8px;
    background: transparent;
    color: var(--color-body);
    cursor: pointer;
    text-align: left;
    width: 100%;
    transition: background 0.1s;
  }

  .session-item:hover {
    background: var(--color-surface-soft);
  }

  .session-item:hover .delete-btn {
    opacity: 0.6;
  }

  .session-item.active {
    background: var(--color-primary-muted);
    border-left: 2px solid var(--color-primary);
    color: var(--color-ink);
  }

  .session-item.focused:not(.active) {
    outline: 2px solid var(--color-primary);
    outline-offset: -2px;
    background: var(--color-surface-soft);
  }

  .session-info {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }

  .session-name {
    font-family: var(--font-serif);
    font-size: 12.5px;
    letter-spacing: 0.06em;
    font-weight: 500;
    line-height: 1.3;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .session-meta {
    display: flex;
    gap: 8px;
    font-size: 11px;
    color: var(--color-muted);
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }

  .delete-btn {
    flex-shrink: 0;
    width: 28px;
    height: 28px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: none;
    border: none;
    color: var(--color-dim);
    border-radius: 6px;
    cursor: pointer;
    opacity: 0;
    transition: opacity 0.15s, background 0.1s;
  }

  .session-item.active .delete-btn {
    opacity: 0.5;
  }

  .delete-btn:hover {
    opacity: 1 !important;
    background: color-mix(in srgb, var(--color-error) 20%, transparent);
    color: var(--color-error);
  }

  .empty-state {
    padding: 24px 12px;
    text-align: center;
    font-size: 13px;
    color: var(--color-muted);
  }

  .session-rename-input {
    width: 100%;
    font: inherit;
    color: inherit;
    background: var(--color-surface-soft);
    border: 1px solid var(--color-primary);
    border-radius: 4px;
    padding: 1px 4px;
  }
</style>
