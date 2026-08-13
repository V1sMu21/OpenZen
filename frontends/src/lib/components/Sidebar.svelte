<script lang="ts">
  import { onMount } from "svelte";
  import { sessions } from "../stores/sessions";
  import { projects } from "../stores/projects";
  import ProjectList from "./ProjectList.svelte";
  import UngroupedSection from "./UngroupedSection.svelte";
  import SidebarFilter from "./SidebarFilter.svelte";
  import { t, locale, switchLocale } from "../i18n";

  let {
    sidebarOpen = $bindable(true),
    onNewChat = $bindable<(projectId?: string) => void>(() => {}),
    onSelectSession = $bindable<(id: string) => void>((_id: string) => {}),
    onSidebarEscape = $bindable<(() => void) | null>(null),
    onAddProject = $bindable<() => void>(() => {}),
  } = $props();

  let filterText = $state("");
  let filterRef: { focus: () => void } | undefined = $state();

  let triggerAddDialog = $state(false);

  function handleAddProject() {
    triggerAddDialog = true;
  }

  function handleQuickNew() {
    onNewChat();
  }

  function handleNewSessionInProject(projectId: string) {
    onNewChat(projectId);
  }

  let ungroupedSessions = $derived(
    $sessions.sessions.filter(
      (s) => !s.project_id || $projects.projects.every((p) => p.sessions.every((ps) => ps.id !== s.id))
    )
  );

  function handleKeydown(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key === "f") {
      e.preventDefault();
      filterRef?.focus();
    }
  }

  onMount(() => {
    sessions.load();
    projects.loadAll();
  });
</script>

<svelte:window onkeydown={handleKeydown} />

<aside class="sidebar" class:open={sidebarOpen}>
  <div class="sidebar-actions">
    <div class="action-row">
      <button class="new-chat-btn glaze-sweep" onclick={handleQuickNew} title={$t("sidebar.newChat")}>
        <span class="plus">＋</span><span>{$t("sidebar.newChat")}</span>
      </button>
      <button class="quick-new-btn" onclick={() => handleAddProject()} title={$t("sidebar.addProject")}>
        <svg width="14" height="14" viewBox="0 0 16 16" fill="none">
          <path d="M8 3v10M3 8h10" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
        </svg>
      </button>
    </div>
  </div>

  <div class="sidebar-divider"></div>

  <SidebarFilter bind:filterText bind:this={filterRef} />

  <div class="sidebar-divider"></div>

  <div class="sidebar-content">
    <div class="side-head">{$t("sidebar.projects")}</div>
    <ProjectList
      bind:filterText
      bind:currentSessionId={$sessions.currentId}
      {onSelectSession}
      onNewSession={handleNewSessionInProject}
      onEscape={onSidebarEscape}
      bind:openAddDialog={triggerAddDialog}
    />

    <div class="sidebar-divider"></div>

    <div class="side-head">{$t("sidebar.sessions")}</div>
    <UngroupedSection
      bind:sessions={ungroupedSessions}
      bind:currentSessionId={$sessions.currentId}
      {onSelectSession}
      onEscape={onSidebarEscape}
    />
  </div>

  <div class="sidebar-footer">
    <div class="side-foot">
      <button class="foot-btn" onclick={() => switchLocale($locale === "zh" ? "en" : "zh")} title={$locale === "zh" ? "Switch to English" : "切换到中文"}>{$locale === "zh" ? "EN" : "中文"}</button>
    </div>
  </div>
</aside>

<style>
  .sidebar {
    width: 0;
    overflow: hidden;
    background: var(--color-canvas);
    border-right: 1px solid var(--color-hairline);
    display: flex;
    flex-direction: column;
    transition: width 0.2s ease;
    flex-shrink: 0;
    box-shadow:
      inset -1px 0 0.5px rgba(129, 181, 199, 0.04),
      inset 0 0 0.5px rgba(129, 181, 199, 0.03);
  }

  .sidebar.open {
    width: 280px;
  }

  .sidebar-actions {
    padding: 8px 12px;
  }

  .action-row {
    display: flex;
    gap: 6px;
  }

  .new-chat-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    flex: 1;
    padding: 7px 0;
    border-radius: 3px;
    background: none;
    color: var(--color-primary);
    border: 1px solid var(--color-primary);
    font-family: var(--font-serif);
    font-size: 12px;
    letter-spacing: 0.2em;
    cursor: pointer;
    transition: background 0.35s var(--ease-soak, ease), color 0.35s var(--ease-soak, ease);
  }

  .new-chat-btn:hover {
    background: var(--color-primary-muted);
    color: var(--color-primary-hover);
  }

  .new-chat-btn .plus {
    font-size: 13px;
    line-height: 1;
  }

  .quick-new-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 34px;
    padding: 6px 0;
    border-radius: 3px;
    background: var(--color-surface-soft);
    color: var(--color-muted);
    border: 1px solid var(--color-hairline);
    font-size: 14px;
    cursor: pointer;
    transition: background 0.2s, color 0.2s;
  }

  .quick-new-btn:hover {
    background: var(--color-surface-elevated);
    color: var(--color-ink);
  }

  .sidebar-divider {
    height: 1px;
    background: var(--color-hairline);
    margin: 4px 12px;
  }

  .sidebar-content {
    flex: 1;
    overflow-y: auto;
    padding: 4px 0;
  }

  .sidebar-footer {
    padding: 8px 12px 12px;
    border-top: 1px solid var(--color-hairline);
  }

  .side-foot {
    display: flex;
    gap: 4px;
  }

  .foot-btn {
    flex: 1;
    text-align: center;
    padding: 6px 0;
    border-radius: 3px;
    border: 1px solid transparent;
    background: none;
    color: var(--color-muted);
    font-family: var(--font-serif);
    font-size: 11px;
    letter-spacing: 0.2em;
    cursor: pointer;
    transition: background 0.3s var(--ease-soak, ease), color 0.3s var(--ease-soak, ease), border-color 0.3s var(--ease-soak, ease);
  }

  .foot-btn:hover {
    background: var(--color-primary-muted);
    color: var(--color-primary);
    border-color: var(--color-hairline);
  }

  /* ── 项目 / 会话 区块标题 (宋体铭文) ── */
  .side-head {
    font-family: var(--font-serif);
    font-size: 11px;
    letter-spacing: 0.3em;
    color: var(--color-dim);
    padding: 2px 10px 6px;
    user-select: none;
  }
</style>
