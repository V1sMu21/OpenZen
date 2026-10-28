<script lang="ts">
  import { sessions } from "../stores/sessions";
  import { projects } from "../stores/projects";
  import ProjectList from "./ProjectList.svelte";
  import UngroupedSection from "./UngroupedSection.svelte";
  import SidebarFilter from "./SidebarFilter.svelte";
  import { t, locale, switchLocale } from "../i18n";
  import { settings } from "../stores/settings.svelte";
  import SettingsPanel from "./SettingsPanel.svelte";
  import ImportSessionsDialog from "./ImportSessionsDialog.svelte";

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
  let importOpen = $state(false);

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
</script>

<svelte:window onkeydown={handleKeydown} />

<aside class="sidebar" class:open={sidebarOpen}>
  <div class="sidebar-actions">
    <div class="action-row">
      <button class="new-chat-btn" onclick={handleQuickNew} title={$t("sidebar.newChat") + " (⌘N)"}>
        <svg width="14" height="14" viewBox="0 0 16 16" fill="none" aria-hidden="true">
          <path d="M8 3v10M3 8h10" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
        </svg>
        <span>{$t("sidebar.newChat")}</span>
        <span class="nc-kbd" aria-hidden="true">⌘N</span>
      </button>
      <button class="quick-new-btn" onclick={() => handleAddProject()} title={$t("sidebar.addProject")} aria-label={$t("sidebar.addProject")}>
        <svg width="14" height="14" viewBox="0 0 16 16" fill="none" aria-hidden="true">
          <path d="M2.5 4.5A1.5 1.5 0 0 1 4 3h2.6l1.2 1.6H12a1.5 1.5 0 0 1 1.5 1.5v5.9A1.5 1.5 0 0 1 12 13.5H4A1.5 1.5 0 0 1 2.5 12z" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/>
          <path d="M8 7v4M6 9h4" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>
        </svg>
      </button>
      <!-- 导入会话 / Import sessions — same sizing + hover as the + button -->
      <button
        class="quick-new-btn import-btn"
        onclick={() => (importOpen = true)}
        title={$t("import.tooltip")}
        aria-label={$t("import.button")}
      >
        <svg width="14" height="14" viewBox="0 0 16 16" fill="none" aria-hidden="true">
          <path d="M8 2v7m0 0L5.4 6.4M8 9l2.6-2.6" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/>
          <path d="M2.5 10.5v2a1 1 0 0 0 1 1h9a1 1 0 0 0 1-1v-2" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>
        </svg>
      </button>
    </div>
  </div>

  <SidebarFilter bind:filterText bind:this={filterRef} />

  <div class="sidebar-content">
    <div class="side-head"><span>{$t("sidebar.projects")}</span></div>
    <ProjectList
      bind:filterText
      bind:currentSessionId={$sessions.currentId}
      {onSelectSession}
      onNewSession={handleNewSessionInProject}
      onEscape={onSidebarEscape}
      bind:openAddDialog={triggerAddDialog}
    />

    <div class="side-head"><span>{$t("sidebar.sessions")}</span></div>
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
      <span class="foot-spacer"></span>
      <!-- 设置入口: 打开右侧设置抽屉 (模型/技能MCP/灵魂/Token) -->
      <button class="foot-btn gear" class:on={settings.open} onclick={() => settings.toggle()} title={$t("settings.title")} aria-label={$t("settings.title")}>
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"/>
          <circle cx="12" cy="12" r="3"/>
        </svg>
      </button>
    </div>
  </div>
</aside>

<SettingsPanel />

<ImportSessionsDialog open={importOpen} onClose={() => (importOpen = false)} />

<style>
  /* ── 侧栏: 陶胎面板, 276px ── */
  .sidebar {
    width: 0;
    overflow: hidden;
    background: var(--color-surface);
    border-right: 1px solid var(--color-hairline);
    display: flex;
    flex-direction: column;
    transition: width 0.26s var(--ease-soak);
    flex-shrink: 0;
  }

  .sidebar.open {
    width: 276px;
  }

  .sidebar-actions {
    padding: 14px 12px 4px;
  }

  .action-row {
    display: flex;
    gap: 6px;
  }

  /* 主行动: 实底深青, 浅字 — 全屏唯一的大面积强调色 */
  .new-chat-btn {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 1;
    min-width: 0;
    height: 38px;
    padding: 0 10px 0 12px;
    border-radius: 11px;
    background: linear-gradient(180deg, #43a996 0%, #2b7f71 100%);
    color: #f0fffb;
    border: 1px solid color-mix(in srgb, #66dccb 42%, transparent);
    font-family: var(--font-sans);
    font-size: 12.5px;
    font-weight: 600;
    letter-spacing: 0.01em;
    cursor: pointer;
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.22),
      0 10px 24px -16px rgba(64, 180, 156, 0.9);
    transition: filter 0.18s var(--ease-soak), transform 0.18s var(--ease-soak);
  }

  .new-chat-btn:hover {
    filter: brightness(1.06);
  }

  .new-chat-btn:active {
    transform: translateY(1px);
  }

  .new-chat-btn span {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .nc-kbd {
    margin-left: auto;
    font-family: var(--font-mono);
    font-size: 9.5px;
    font-weight: 500;
    letter-spacing: 0;
    padding: 2px 5px;
    border-radius: 5px;
    background: rgba(4, 17, 14, 0.16);
    color: rgba(240, 255, 251, 0.72);
  }

  .quick-new-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 38px;
    height: 38px;
    flex: none;
    border-radius: 11px;
    background: var(--color-surface-soft);
    color: var(--color-dim);
    border: 1px solid var(--color-hairline);
    cursor: pointer;
    transition: background 0.18s, color 0.18s, border-color 0.18s;
  }

  .quick-new-btn:hover {
    background: var(--color-surface-elevated);
    color: var(--color-ink);
    border-color: var(--color-hairline-strong);
  }

  .quick-new-btn.import-btn:focus-visible {
    outline: 2px solid var(--color-primary);
    outline-offset: 2px;
  }

  .sidebar-content {
    flex: 1;
    overflow-y: auto;
    padding: 4px 8px 16px;
  }

  .sidebar-footer {
    padding: 8px 10px 10px;
    border-top: 1px solid var(--color-hairline);
  }

  .side-foot {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .foot-spacer {
    flex: 1;
  }

  .foot-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 30px;
    height: 28px;
    padding: 0 8px;
    border-radius: 8px;
    border: 1px solid transparent;
    background: none;
    color: var(--color-muted);
    font-family: var(--font-mono);
    font-size: 10px;
    letter-spacing: 0.08em;
    cursor: pointer;
    transition: background 0.18s, color 0.18s;
  }

  .foot-btn:hover {
    background: var(--color-surface-soft);
    color: var(--color-ink);
  }

  .foot-btn.gear.on {
    color: var(--color-primary);
    background: var(--color-primary-soft);
  }

  /* ── 区块标题: 小字 + 延伸发丝线 ── */
  .side-head {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 18px 12px 8px;
    user-select: none;
  }

  .side-head span {
    font-family: var(--font-sans);
    font-size: 10px;
    font-weight: 600;
    letter-spacing: 0.16em;
    color: var(--color-muted);
    white-space: nowrap;
  }

  .side-head::after {
    content: "";
    flex: 1;
    height: 1px;
    background: var(--color-hairline);
  }
</style>
