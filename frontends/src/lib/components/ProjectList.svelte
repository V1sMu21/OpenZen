<script lang="ts">
  import { projects } from "../stores/projects";
  import { sessions } from "../stores/sessions";
  import ProjectItem from "./ProjectItem.svelte";
  import { t } from "../i18n";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { isTauri, tauriInvoke } from "../api/tauri";

  let {
    filterText = $bindable(""),
    currentSessionId = $bindable<string | null>(null),
    onSelectSession = $bindable<(id: string) => void>(() => {}),
    onNewSession = $bindable<(projectId?: string) => void>(() => {}),
    onEscape = $bindable<(() => void) | null>(null),
    openAddDialog = $bindable(false),
  } = $props();

  let isFiltering = $derived(filterText.trim().length > 0);

  let showAddDialog = $state(false);
  $effect(() => {
    if (openAddDialog) {
      showAddDialog = true;
      openAddDialog = false;
    }
  });
  let addPath = $state("");
  let addName = $state("");
  let addError = $state("");

  async function handleAddProject() {
    if (isTauri()) {
      try {
        const selected = await openDialog({ directory: true, multiple: false, title: "Select project folder" });
        if (selected && typeof selected === "string") {
          addPath = selected;
        }
      } catch {
        addPath = "";
      }
    }
    if (addPath) {
      await projects.add(addPath, addName || undefined);
      showAddDialog = false;
      addPath = "";
      addName = "";
      addError = "";
    } else {
      showAddDialog = true;
    }
  }

  async function handleDialogConfirm() {
    if (!addPath.trim()) {
      addError = "Please select a folder";
      return;
    }
    try {
      await projects.add(addPath.trim(), addName.trim() || undefined);
      showAddDialog = false;
      addPath = "";
      addName = "";
      addError = "";
    } catch (e: unknown) {
      addError = e instanceof Error ? e.message : String(e);
    }
  }

  async function handleNewSessionInProject(projectId: string) {
    const result = await projects.createSessionIn(projectId);
    console.log("[ProjectList] new session in project:", projectId, "=> session:", result?.session_id);
    if (result) {
      onSelectSession(result.session_id);
    }
  }

  async function handleRemoveProject(projectId: string) {
    await projects.remove(projectId);
  }

  async function handleRenameProject(projectId: string, newName: string) {
    try {
      await projects.rename(projectId, newName);
    } catch (e) {
      console.error("[ProjectList] rename failed:", e);
    }
  }

  async function handleOpenFinder(projectId: string) {
    const p = $projects.projects.find((p) => p.id === projectId);
    if (!p || !isTauri()) return;
    try {
      await tauriInvoke("reveal_in_finder", { path: p.root_path });
    } catch (e) {
      console.error("[ProjectList] open in Finder failed:", e);
    }
  }

  function isProjectDimmed(p: { sessions: { name?: string }[]; name: string }, text: string): boolean {
    if (!text) return false;
    const q = text.toLowerCase();
    const nameMatch = p.name.toLowerCase().includes(q);
    const sessionMatch = p.sessions.some((s) => (s.name ?? "").toLowerCase().includes(q));
    return !nameMatch && !sessionMatch;
  }
</script>

<div class="project-list">
  {#each $projects.projects as project (project.id)}
    <ProjectItem
      {project}
      isExpanded={$projects.expandedProjectIds.has(project.id)}
      isDimmed={isProjectDimmed(project, filterText)}
      activeProjectId={null}
      bind:currentSessionId
      bind:filterText
      {onSelectSession}
      onNewSession={handleNewSessionInProject}
      onRename={handleRenameProject}
      onRemove={handleRemoveProject}
      onOpenFinder={handleOpenFinder}
      {onEscape}
    />
  {/each}

  {#if $projects.projects.length === 0 && !$projects.loading}
    <div class="empty-projects">
      <p>{$t("project.emptyHint")}</p>
    </div>
  {/if}
</div>

{#if showAddDialog}
  <div class="dialog-overlay" onclick={() => showAddDialog = false}>
    <div class="dialog" onclick={(e) => e.stopPropagation()}>
      <h3>{$t("project.addDialogTitle")}</h3>
      {#if isTauri()}
        <div class="dialog-field">
          <label>{$t("project.folderLabel")}</label>
          <div class="path-row">
            <input type="text" bind:value={addPath} placeholder="/path/to/project" />
            <button class="browse-btn" onclick={async () => {
              try {
                const selected = await openDialog({ directory: true, multiple: false, title: "Select project folder" });
                if (selected && typeof selected === "string") addPath = selected;
              } catch {}
            }}>
              {$t("project.browse")}
            </button>
          </div>
        </div>
        <div class="dialog-field">
          <label>{$t("project.nameLabel")}</label>
          <input type="text" bind:value={addName} placeholder={$t("project.namePlaceholder")} />
        </div>
      {:else}
        <div class="dialog-field">
          <label>{$t("project.folderLabel")}</label>
          <input type="text" bind:value={addPath} placeholder="/path/to/project" />
        </div>
      {/if}
      {#if addError}
        <p class="dialog-error">{addError}</p>
      {/if}
      <div class="dialog-actions">
        <button class="btn-cancel" onclick={() => showAddDialog = false}>{$t("project.cancel")}</button>
        <button class="btn-confirm" onclick={handleDialogConfirm}>{$t("project.add")}</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .project-list {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .empty-projects {
    padding: 24px 16px;
    text-align: center;
  }

  .empty-projects p {
    font-size: 13px;
    color: var(--color-muted);
    margin: 0;
    line-height: 1.5;
  }

  .dialog-overlay {
    position: fixed;
    inset: 0;
    z-index: 200;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .dialog {
    background: var(--color-canvas);
    border-radius: 12px;
    padding: 24px;
    width: 400px;
    max-width: 90vw;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.2);
  }

  .dialog h3 {
    margin: 0 0 16px;
    font-size: 16px;
    font-weight: 600;
    color: var(--color-ink);
  }

  .dialog-field {
    margin-bottom: 12px;
  }

  .dialog-field label {
    display: block;
    font-size: 12px;
    color: var(--color-muted);
    margin-bottom: 4px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }

  .dialog-field input {
    width: 100%;
    padding: 8px 10px;
    border: 1px solid var(--color-hairline);
    border-radius: 6px;
    font-size: 13px;
    background: var(--color-surface-soft);
    color: var(--color-ink);
    box-sizing: border-box;
  }

  .path-row {
    display: flex;
    gap: 8px;
  }

  .path-row input {
    flex: 1;
  }

  .browse-btn {
    padding: 8px 12px;
    border: 1px solid var(--color-hairline);
    border-radius: 6px;
    background: var(--color-surface-soft);
    color: var(--color-ink);
    font-size: 12px;
    cursor: pointer;
    white-space: nowrap;
  }

  .browse-btn:hover {
    background: var(--color-surface-elevated);
  }

  .dialog-error {
    color: var(--color-error);
    font-size: 12px;
    margin: 8px 0;
  }

  .dialog-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  }

  .btn-cancel {
    padding: 8px 16px;
    border: 1px solid var(--color-hairline);
    border-radius: 8px;
    background: var(--color-surface-soft);
    color: var(--color-muted);
    font-size: 13px;
    cursor: pointer;
  }

  .btn-confirm {
    padding: 8px 16px;
    border: none;
    border-radius: 8px;
    background: var(--color-primary);
    color: white;
    font-size: 13px;
    font-weight: 500;
    cursor: pointer;
  }

  .btn-confirm:hover {
    background: var(--color-primary-hover);
  }
</style>
