<script lang="ts">
  import type { ProjectWithSessions } from "../api/projects";
  import { projects } from "../stores/projects";
  import SessionList from "./SessionList.svelte";
  import { t } from "../i18n";

  let {
    project = $bindable({} as ProjectWithSessions),
    isExpanded = $bindable(false),
    isDimmed = $bindable(false),
    activeProjectId = $bindable<string | null>(null),
    currentSessionId = $bindable<string | null>(null),
    filterText = $bindable(""),
    onSelectSession = $bindable<(id: string) => void>(() => {}),
    onNewSession = $bindable<(projectId: string) => void>(() => {}),
    onRename = $bindable<(projectId: string, name: string) => void>(() => {}),
    onRemove = $bindable<(projectId: string) => void>(() => {}),
    onOpenFinder = $bindable<(projectId: string) => void>(() => {}),
    onEscape = $bindable<(() => void) | null>(null),
  } = $props();

  let showMenu = $state(false);
  let menuX = $state(0);
  let menuY = $state(0);
  let isHover = $state(false);
  let renaming = $state(false);
  let renameValue = $state("");
  let renameInputEl = $state<HTMLInputElement | undefined>(undefined);

  function toggleExpand() {
    if (!isDimmed) {
      isExpanded = !isExpanded;
    }
  }

  function handleContextMenu(e: MouseEvent) {
    e.preventDefault();
    menuX = e.clientX;
    menuY = e.clientY;
    showMenu = true;
  }

  function hideMenu() {
    showMenu = false;
  }

  function handleMenuAction(action: () => void) {
    action();
    hideMenu();
  }

  function startRename() {
    renaming = true;
    renameValue = project.name;
    requestAnimationFrame(() => {
      renameInputEl?.focus();
      renameInputEl?.select();
    });
  }

  function commitRename() {
    const trimmed = renameValue.trim();
    if (trimmed && trimmed !== project.name) {
      onRename(project.id, trimmed);
    }
    renaming = false;
  }

  function handleRenameKeydown(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === "Enter") {
      e.preventDefault();
      commitRename();
    } else if (e.key === "Escape") {
      e.preventDefault();
      renaming = false;
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "ArrowRight" || e.key === "Enter") {
      e.preventDefault();
      if (!isExpanded) isExpanded = true;
    } else if (e.key === "ArrowLeft") {
      e.preventDefault();
      if (isExpanded) isExpanded = false;
    }
  }

  $effect(() => {
    if (showMenu) {
      const close = (e: MouseEvent) => {
        const target = e.target as HTMLElement;
        if (!target.closest(".project-context-menu")) hideMenu();
      };
      window.addEventListener("click", close);
      return () => window.removeEventListener("click", close);
    }
  });
</script>

<div
  class="project-item"
  class:expanded={isExpanded}
  class:dimmed={isDimmed}
  class:active={activeProjectId === project.id}
  class:hover={isHover}
  role="treeitem"
  aria-expanded={isExpanded}
  onmouseenter={() => isHover = true}
  onmouseleave={() => isHover = false}
  onkeydown={handleKeydown}
  tabindex="0"
>
  <div class="project-row" onclick={toggleExpand} oncontextmenu={handleContextMenu}>
    <span class="project-chevron">{isExpanded ? "▾" : "▸"}</span>
    {#if project.broken}
      <span class="project-icon">⚠️</span>
    {/if}
    <span class="project-name" class:hidden={renaming}>{project.name}</span>
    {#if renaming}
      <input
        bind:this={renameInputEl}
        bind:value={renameValue}
        class="project-rename-input"
        onclick={(e) => e.stopPropagation()}
        onkeydown={handleRenameKeydown}
        onblur={() => { if (renaming) renaming = false; }}
        aria-label="Rename project"
      />
    {/if}
    <span class="project-count">{project.sessions?.length ?? 0}</span>
  </div>

  {#if isExpanded && !isDimmed && project.sessions?.length > 0}
    <div class="project-sessions">
      <SessionList
        bind:items={project.sessions}
        bind:currentId={currentSessionId}
        {onSelectSession}
        {onEscape}
        indentLevel={1}
      />
    </div>
  {/if}

  {#if showMenu}
    <div class="project-context-menu" style="left:{menuX}px;top:{menuY}px">
      <button onclick={() => handleMenuAction(() => onNewSession(project.id))}>
        {$t("project.newSession")}
      </button>
      <button onclick={() => handleMenuAction(() => startRename())}>
        {$t("project.rename")}
      </button>
      <button onclick={() => handleMenuAction(() => onOpenFinder(project.id))}>
        {$t("project.openInFinder")}
      </button>
      <hr />
      <button class="danger" onclick={() => handleMenuAction(() => onRemove(project.id))}>
        {$t("project.remove")}
      </button>
    </div>
  {/if}
</div>

<style>
  .project-item {
    margin: 0 8px;
    border-radius: 8px;
    transition: opacity 0.15s;
  }

  .project-item.dimmed {
    opacity: 0.4;
    pointer-events: none;
  }

  .project-item.active {
    background: var(--color-primary-muted);
  }

  .project-item.hover:not(.active) .project-row {
    background: var(--color-surface-soft);
  }

  .project-row {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 12px;
    border-radius: 8px;
    cursor: pointer;
    user-select: none;
  }

  .project-chevron {
    font-size: 10px;
    color: var(--color-body);
    width: 12px;
    text-align: center;
  }

  .project-icon {
    font-size: 13px;
  }

  .project-name {
    flex: 1;
    font-size: 13px;
    font-weight: 600;
    color: var(--color-ink);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .project-name.hidden {
    display: none;
  }

  .project-rename-input {
    flex: 1;
    min-width: 0;
    font-size: 13px;
    font-weight: 600;
    color: var(--color-ink);
    background: var(--color-surface-soft);
    border: 1px solid var(--color-primary);
    border-radius: 4px;
    padding: 1px 6px;
    outline: none;
  }

  .project-count {
    font-size: 11px;
    color: var(--color-body);
    background: var(--color-surface-soft);
    padding: 1px 6px;
    border-radius: 10px;
    font-weight: 500;
  }

  .project-sessions {
    padding-left: 12px;
  }

  .project-context-menu {
    position: fixed;
    z-index: 100;
    background: var(--color-surface-elevated);
    border: 1px solid var(--color-hairline);
    border-radius: 8px;
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.15);
    padding: 4px;
    min-width: 160px;
  }

  .project-context-menu button {
    display: block;
    width: 100%;
    padding: 8px 12px;
    border: none;
    background: none;
    text-align: left;
    font-size: 13px;
    color: var(--color-ink);
    border-radius: 6px;
    cursor: pointer;
  }

  .project-context-menu button:hover {
    background: var(--color-primary-muted);
  }

  .project-context-menu button.danger {
    color: var(--color-error);
  }

  .project-context-menu button.danger:hover {
    background: color-mix(in srgb, var(--color-error) 15%, transparent);
  }

  .project-context-menu hr {
    margin: 4px 8px;
    border: none;
    border-top: 1px solid var(--color-hairline);
  }
</style>
