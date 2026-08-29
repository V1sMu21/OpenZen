<script lang="ts">
  import { settings } from "../stores/settings.svelte";
  import { t } from "../i18n";
  import { formatTokenCount } from "../stores/types";
  import type {
    McpServerItem,
    ModelEntry,
    SkillMcpItem,
    TokenStats,
  } from "../api/chat";
  import {
    birthNameDisplay,
    deleteModel,
    fetchModels,
    getTokenStats,
    listMcpServers,
    listSkillMcp,
    setDefaultModel,
    setSoulIdentity,
    toggleMcpServer,
    toggleSkillMcp,
    upsertModel,
  } from "../api/settings";
  import { soulDisplayName } from "../api/settings";
  import { soulStore } from "../stores/soul.svelte";

  type Tab = "models" | "skills" | "soul" | "tokens";

  let tab = $state<Tab>("models");
  let loading = $state(false);
  let error = $state("");

  // ── models tab ──
  let models = $state<ModelEntry[]>([]);
  /** null = list view; otherwise the inline create/edit form. */
  let editing = $state<{
    name: string;
    isNew: boolean;
    apibase: string;
    apikey: string;
    model: string;
    context_win: number;
  } | null>(null);

  // ── skills tab ──
  let skills = $state<SkillMcpItem[]>([]);
  let sops = $state<SkillMcpItem[]>([]);
  let servers = $state<McpServerItem[]>([]);

  // ── soul tab ──
  // Shared store: renaming here also updates the title bar and the rail
  // SoulCard instantly (they read the same signal).
  let soul = $derived(soulStore.status);
  let nameDraft = $state("");
  let renaming = $state(false);
  let savingName = $state(false);

  // ── tokens tab ──
  let stats = $state<TokenStats | null>(null);

  /** Last 7 days of perDay data, oldest → newest (perDay is sorted by day). */
  let recentDays = $derived.by(() => {
    const days = [...(stats?.perDay ?? [])].sort((a, b) => a.day.localeCompare(b.day));
    return days.slice(-7);
  });
  let maxDayTokens = $derived.by(() =>
    Math.max(1, ...recentDays.map((d) => d.in + d.out)),
  );

  /** Runs fn with the shared loading/error banner; true on success. */
  async function run(fn: () => Promise<void>): Promise<boolean> {
    loading = true;
    error = "";
    try {
      await fn();
      return true;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      return false;
    } finally {
      loading = false;
    }
  }

  function loadTab() {
    if (tab === "models") {
      void run(async () => {
        models = await fetchModels();
      });
    } else if (tab === "skills") {
      void run(async () => {
        const [list, sv] = await Promise.all([listSkillMcp(), listMcpServers()]);
        skills = list.skills;
        sops = list.sops;
        servers = sv.servers;
      });
    } else if (tab === "soul") {
      void run(async () => {
        await soulStore.load();
      });
    } else {
      void run(async () => {
        stats = await getTokenStats(50);
      });
    }
  }

  // Reload whenever the panel opens or the tab changes.
  $effect(() => {
    if (!settings.open) return;
    loadTab();
  });

  function startCreate() {
    editing = { name: "", isNew: true, apibase: "", apikey: "", model: "", context_win: 28000 };
  }

  function startEdit(m: ModelEntry) {
    editing = {
      name: m.name,
      isNew: false,
      // apibase/apikey are not in ModelEntry (list view only carries the
      // summary); the form leaves them blank and the backend keeps the
      // stored values.
      apibase: "",
      apikey: "",
      model: m.model,
      context_win: m.context_win,
    };
  }

  async function saveModel() {
    if (!editing) return;
    const ed = editing;
    // Editing an existing entry may leave apibase/model blank — the backend
    // keeps the stored values; a brand-new entry needs both.
    if (!ed.name.trim() || (ed.isNew && (!ed.apibase.trim() || !ed.model.trim()))) {
      error = $t("settings.model.required");
      return;
    }
    await run(async () => {
      await upsertModel({
        name: ed.name.trim(),
        apibase: ed.apibase.trim() || undefined,
        apikey: ed.apikey.trim() || undefined,
        model: ed.model.trim() || undefined,
        context_win: ed.context_win,
      });
      models = await fetchModels();
      editing = null;
    });
  }

  async function removeModel(name: string) {
    if (!window.confirm(`${$t("settings.model.confirmDelete")} ${name}`)) return;
    await run(async () => {
      await deleteModel(name);
      models = await fetchModels();
    });
  }

  async function makeDefault(name: string) {
    await run(async () => {
      await setDefaultModel(name);
      models = await fetchModels();
    });
  }

  async function flipSkill(kind: "skill" | "sop", name: string, active: boolean) {
    const ok = await run(async () => {
      await toggleSkillMcp(kind, name, active);
    });
    if (ok) {
      // Keep the checkbox in sync with the persisted value on success…
      const item = (kind === "skill" ? skills : sops).find((s) => s.name === name);
      if (item) item.active = active;
    } else {
      // …and resync from disk on failure so a rejected toggle doesn't leave
      // the checkbox showing the opposite of the stored state.
      try {
        const list = await listSkillMcp();
        skills = list.skills;
        sops = list.sops;
      } catch {
        // the error banner already shows the toggle failure cause
      }
    }
  }

  async function flipServer(name: string, enabled: boolean) {
    const ok = await run(async () => {
      await toggleMcpServer(name, enabled);
    });
    if (ok) {
      const item = servers.find((sv) => sv.name === name);
      if (item) item.enabled = enabled;
    } else {
      try {
        servers = (await listMcpServers()).servers;
      } catch {
        // the error banner already shows the toggle failure cause
      }
    }
  }

  async function saveName() {
    const name = nameDraft.trim();
    if (!name || savingName) return;
    savingName = true;
    try {
      await setSoulIdentity(name);
      await soulStore.load();
      renaming = false;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      savingName = false;
    }
  }

  function focusOnMount(node: HTMLInputElement) {
    // 改名输入框必须自动聚焦 — 否则用户直接打字落在页面上，
    // "起了名字没生效"（E2E 实测踩过的坑）
    node.focus();
    node.select();
  }

  function startRename() {
    // 只预填用户起过的名字；诞生名（记忆体 · 醒于 …）留空
    nameDraft = soulDisplayName(soul) ?? "";
    renaming = true;
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape" && settings.open) settings.close();
  }
</script>

{#snippet toggleRow(
  item: SkillMcpItem | McpServerItem,
  checked: boolean,
  desc: string,
  flip: (name: string, on: boolean) => void,
  extra?: string,
)}
  <div class="skill-row">
    <label class="skill-info">
      <input
        type="checkbox"
        checked={checked}
        onchange={(e) => flip(item.name, e.currentTarget.checked)}
      />
      <span class="skill-name">{item.name}</span>
      <span class="skill-desc" title={desc}>{desc}</span>
    </label>
    {#if extra}<span class="skill-quality">{extra}</span>{/if}
  </div>
{/snippet}

<svelte:window onkeydown={onKeydown} />

{#if settings.open}
  <div class="settings-backdrop" onclick={() => settings.close()} aria-hidden="true"></div>
  <aside class="settings-panel" role="dialog" aria-label={$t("settings.title")}>
    <header class="settings-head">
      <span class="settings-title">{$t("settings.title")}</span>
      <button class="head-close" onclick={() => settings.close()} aria-label={$t("settings.close")}>✕</button>
    </header>

    <nav class="settings-tabs">
      {#each [["models", "settings.tab.models"], ["skills", "settings.tab.skills"], ["soul", "settings.tab.soul"], ["tokens", "settings.tab.tokens"]] as [key, label] (key)}
        <button class="settings-tab" class:on={tab === key} onclick={() => (tab = key as Tab)}>
          {$t(label)}
        </button>
      {/each}
    </nav>

    {#if error}<div class="settings-error">{error}</div>{/if}
    {#if loading}<div class="settings-loading">{$t("settings.loading")}</div>{/if}

    <div class="settings-body">
      {#if tab === "models"}
        {#if editing === null}
          <div class="section-row">
            <button class="btn primary" onclick={startCreate}>{$t("settings.model.new")}</button>
          </div>
          {#each models as m (m.name)}
            <div class="model-row">
              <div class="model-info">
                <span class="model-name">
                  <span class="model-name-text" title={m.name}>{m.name}</span>
                  {#if m.is_default}<span class="tag default">{$t("settings.model.isDefault")}</span>{/if}
                </span>
                <span class="model-meta">{m.model} · {$t(m.is_local ? "status.localDeploy" : "status.cloud")} · {m.context_win}</span>
              </div>
              <div class="model-actions">
                {#if !m.is_default}
                  <button class="btn ghost" onclick={() => makeDefault(m.name)}>{$t("settings.model.setDefault")}</button>
                {/if}
                <button class="btn ghost" onclick={() => startEdit(m)}>{$t("settings.edit")}</button>
                <button class="btn ghost danger" onclick={() => removeModel(m.name)}>{$t("settings.delete")}</button>
              </div>
            </div>
          {:else}
            <div class="settings-empty">{$t("settings.empty")}</div>
          {/each}
        {:else}
          <div class="form">
            <label class="field">
              <span>{$t("settings.model.name")}</span>
              <input bind:value={editing.name} disabled={!editing.isNew} placeholder="Agents_A1_8bit" />
            </label>
            <label class="field">
              <span>{$t("settings.model.apibase")}</span>
              <input bind:value={editing.apibase} placeholder={editing.isNew ? "http://127.0.0.1:8000/v1" : ""} />
            </label>
            <label class="field">
              <span>{$t("settings.model.apikey")}</span>
              <input bind:value={editing.apikey} type="password" placeholder={editing.isNew ? "" : $t("settings.model.apikeyHint")} />
            </label>
            <label class="field">
              <span>{$t("settings.model.modelId")}</span>
              <input bind:value={editing.model} />
            </label>
            <label class="field">
              <span>{$t("settings.model.contextWin")}</span>
              <input type="number" bind:value={editing.context_win} min={1000} step={1000} />
            </label>
            <div class="form-actions">
              <button class="btn primary" onclick={saveModel} disabled={loading}>{$t("settings.save")}</button>
              <button class="btn ghost" onclick={() => (editing = null)}>{$t("settings.cancel")}</button>
            </div>
            {#if !editing.isNew}
              <p class="form-hint">{$t("settings.model.editHint")}</p>
            {/if}
          </div>
        {/if}
      {:else if tab === "skills"}
        <div class="group">
          <div class="group-head">{$t("settings.skills.list")} ({skills.length})</div>
          {#each skills as s (s.name)}
            {@render toggleRow(s, s.active, s.description, (n, on) => flipSkill("skill", n, on), `${Math.round((s.quality ?? 0) * 100)}%`)}
          {:else}
            <div class="settings-empty">{$t("settings.empty")}</div>
          {/each}
        </div>
        <div class="group">
          <div class="group-head">{$t("settings.skills.sops")} ({sops.length})</div>
          {#each sops as s (s.name)}
            {@render toggleRow(s, s.active, s.description, (n, on) => flipSkill("sop", n, on))}
          {:else}
            <div class="settings-empty">{$t("settings.empty")}</div>
          {/each}
        </div>
        <div class="group">
          <div class="group-head">{$t("settings.skills.mcp")} ({servers.length})</div>
          {#each servers as sv (sv.name)}
            {@render toggleRow(sv, sv.enabled, sv.command, (n, on) => flipServer(n, on))}
          {:else}
            <div class="settings-empty">{$t("settings.empty")}</div>
          {/each}
          <p class="form-hint">{$t("settings.skills.note")}</p>
        </div>
      {:else if tab === "soul"}
        {#if soul?.enabled && soul.soul}
          <div class="soul-head">
            {#if renaming}
              <input
                class="soul-input"
                bind:value={nameDraft}
                use:focusOnMount
                placeholder={$t("soul.namePlaceholder")}
                maxlength={24}
                disabled={savingName}
                onkeydown={(e) => {
                  if (e.key === "Enter") saveName();
                  else if (e.key === "Escape") renaming = false;
                }}
                onblur={() => {
                  // 失焦也保存 — 只按 Enter 会让人 "输了名字没生效"
                  if (renaming) saveName();
                }}
              />
              <button class="btn primary" onclick={saveName} disabled={savingName}>{$t("settings.save")}</button>
            {:else}
              <span class="soul-name" title={soul.soul.identity}>
                {birthNameDisplay(soul.soul.identity, $t("soul.birthName")) ?? soul.soul.identity}
              </span>
              <button class="btn ghost" onclick={startRename}>{$t("soul.rename")}</button>
            {/if}
          </div>
          <div class="stat-grid">
            <span class="stat-key">{$t("soul.mood")}</span><span class="stat-val">{soul.soul.mood}</span>
            <span class="stat-key">{$t("soul.confidence")}</span><span class="stat-val">{Math.round(soul.soul.confidence * 100)}%</span>
            <span class="stat-key">{$t("soul.memories")}</span><span class="stat-val">{soul.store?.total_entries ?? 0}</span>
            <span class="stat-key">{$t("soul.recalls")}</span><span class="stat-val">{soul.store?.recalls ?? 0} · {$t("soul.hitRate")} {Math.round((soul.store?.recall_hit_rate ?? 0) * 100)}%</span>
            <span class="stat-key">{$t("soul.portraitFacts")}</span><span class="stat-val">{soul.soul.portrait_facts}</span>
            <span class="stat-key">{$t("soul.narrative")}</span><span class="stat-val">{soul.soul.narrative_chapters} {$t("soul.chapters")}</span>
          </div>
        {:else}
          <div class="settings-empty">{$t("settings.empty")}</div>
        {/if}
      {:else}
        {#if stats}
          <div class="token-totals">
            <span>{$t("status.in")} <b>{formatTokenCount(stats.totals.in)}</b></span>
            <span>{$t("status.out")} <b>{formatTokenCount(stats.totals.out)}</b></span>
          </div>
          <div class="group">
            <div class="group-head">{$t("settings.tokens.perDay")}</div>
            {#each recentDays as d (d.day)}
              <div class="bar-row">
                <span class="bar-label">{d.day.slice(5)}</span>
                <span class="bar-track"><span class="bar-fill" style="width:{Math.round(((d.in + d.out) / maxDayTokens) * 100)}%"></span></span>
                <span class="bar-num">{formatTokenCount(d.in + d.out)}</span>
              </div>
            {:else}
              <div class="settings-empty">{$t("settings.empty")}</div>
            {/each}
          </div>
          <div class="group">
            <div class="group-head">{$t("settings.tokens.perModel")}</div>
            {#each stats.perModel.slice(0, 6) as pm (pm.model)}
              <div class="bar-row">
                <span class="bar-label wide" title={pm.model}>{pm.model}</span>
                <span class="bar-num">{formatTokenCount(pm.in + pm.out)}</span>
              </div>
            {:else}
              <div class="settings-empty">{$t("settings.empty")}</div>
            {/each}
          </div>
          <div class="group">
            <div class="group-head">{$t("settings.tokens.perSession")}</div>
            {#each stats.perSession.slice(0, 8) as ps (ps.id)}
              <div class="session-row">
                <span class="session-name" title={ps.name}>{ps.name || ps.id.slice(0, 8)}</span>
                <span class="bar-num">{formatTokenCount(ps.tokensIn + ps.tokensOut)}</span>
              </div>
            {:else}
              <div class="settings-empty">{$t("settings.empty")}</div>
            {/each}
          </div>
        {/if}
      {/if}
    </div>
  </aside>
{/if}

<style>
  .settings-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(20, 18, 14, 0.35);
    z-index: 90;
  }

  .settings-panel {
    position: fixed;
    top: 0;
    right: 0;
    bottom: 0;
    width: 380px;
    max-width: 92vw;
    background: var(--color-canvas);
    border-left: 1px solid var(--color-hairline);
    box-shadow: -8px 0 24px rgba(0, 0, 0, 0.18);
    z-index: 91;
    display: flex;
    flex-direction: column;
  }

  .settings-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 14px;
    border-bottom: 1px solid var(--color-hairline);
  }

  .settings-title {
    font-family: var(--font-serif);
    font-size: 13px;
    letter-spacing: 0.25em;
    color: var(--color-ink);
  }

  .head-close {
    border: none;
    background: none;
    color: var(--color-muted);
    cursor: pointer;
    font-size: 13px;
    padding: 4px 6px;
  }

  .head-close:hover {
    color: var(--color-ink);
  }

  .settings-tabs {
    display: flex;
    gap: 2px;
    padding: 8px 10px 0;
  }

  .settings-tab {
    flex: 1;
    border: 1px solid transparent;
    border-bottom: none;
    background: none;
    padding: 6px 0;
    font-family: inherit;
    font-size: 11.5px;
    color: var(--color-muted);
    cursor: pointer;
    border-radius: 6px 6px 0 0;
  }

  .settings-tab.on {
    color: var(--color-primary);
    border-color: var(--color-hairline);
    background: var(--color-surface-soft);
  }

  .settings-error {
    margin: 8px 14px 0;
    padding: 6px 10px;
    border-radius: 6px;
    font-size: 11.5px;
    color: var(--color-error, #c05a3e);
    background: color-mix(in srgb, var(--color-error, #c05a3e) 10%, transparent);
    word-break: break-word;
  }

  .settings-loading {
    padding: 6px 14px;
    font-size: 11px;
    color: var(--color-muted);
  }

  .settings-body {
    flex: 1;
    overflow-y: auto;
    padding: 12px 14px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .section-row {
    display: flex;
    justify-content: flex-end;
  }

  .btn {
    border: 1px solid var(--color-hairline-strong);
    background: none;
    border-radius: 5px;
    padding: 4px 10px;
    font-family: inherit;
    font-size: 11.5px;
    color: var(--color-ink);
    cursor: pointer;
    transition: background 0.15s, color 0.15s;
  }

  .btn:hover {
    background: var(--color-surface-soft);
  }

  .btn.primary {
    background: var(--color-primary);
    border-color: var(--color-primary);
    color: #14120e;
  }

  .btn.primary:hover {
    background: var(--color-primary-hover);
  }

  .btn.ghost {
    border-color: transparent;
    color: var(--color-muted);
  }

  .btn.ghost:hover {
    color: var(--color-ink);
    border-color: var(--color-hairline);
  }

  .btn.danger:hover {
    color: var(--color-error, #c05a3e);
  }

  .model-row,
  .skill-row,
  .session-row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 7px 8px;
    border-radius: 6px;
  }

  .model-row:hover,
  .skill-row:hover,
  .session-row:hover {
    background: var(--color-surface-soft);
  }

  .model-info {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
    flex: 1;
  }

  .model-name {
    font-size: 12.5px;
    color: var(--color-body);
    display: flex;
    align-items: center;
    gap: 6px;
    /* 截断链: 长模型名（无空格不可断行）必须能收缩, 否则会溢出盖住
       右侧的 设为默认/编辑/删除 按钮 */
    min-width: 0;
  }

  .model-name-text {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tag {
    flex: none;
    font-size: 9.5px;
    padding: 0 5px;
    border-radius: 999px;
    border: 1px solid var(--color-primary);
    color: var(--color-primary);
  }

  .model-meta {
    font-size: 10.5px;
    color: var(--color-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .model-actions {
    display: flex;
    gap: 2px;
    flex: none;
  }

  .group {
    border: 1px solid var(--color-hairline);
    border-radius: 6px;
    background: var(--color-surface-soft);
    padding: 6px 4px;
  }

  .group-head {
    font-size: 10.5px;
    letter-spacing: 0.12em;
    color: var(--color-dim);
    padding: 2px 8px 6px;
  }

  .skill-info {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
    flex: 1;
    cursor: pointer;
  }

  .skill-info input {
    flex: none;
    accent-color: var(--color-primary);
  }

  .skill-name {
    font-size: 12px;
    color: var(--color-body);
    white-space: nowrap;
  }

  .skill-desc {
    font-size: 10.5px;
    color: var(--color-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .skill-quality {
    font-size: 10.5px;
    color: var(--color-muted);
    font-variant-numeric: tabular-nums;
  }

  .form {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 11px;
    color: var(--color-muted);
  }

  .field input {
    font-family: inherit;
    font-size: 12.5px;
    padding: 6px 8px;
    border: 1px solid var(--color-hairline-strong);
    border-radius: 5px;
    background: var(--color-surface-soft);
    color: var(--color-ink);
  }

  .field input:focus {
    outline: none;
    border-color: var(--color-primary);
  }

  .form-actions {
    display: flex;
    gap: 8px;
  }

  .form-hint {
    font-size: 10.5px;
    color: var(--color-muted);
    line-height: 1.5;
  }

  .soul-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .soul-name {
    font-family: var(--font-serif);
    font-size: 16px;
    color: var(--color-ink);
    letter-spacing: 0.08em;
  }

  .soul-input {
    flex: 1;
    font-family: inherit;
    font-size: 13px;
    padding: 5px 8px;
    border: 1px solid var(--color-hairline-strong);
    border-radius: 5px;
    background: var(--color-surface-soft);
    color: var(--color-ink);
  }

  .soul-input:focus {
    outline: none;
    border-color: var(--color-primary);
  }

  .stat-grid {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 7px 14px;
    border: 1px solid var(--color-hairline);
    border-radius: 6px;
    background: var(--color-surface-soft);
    padding: 10px 12px;
  }

  .stat-key {
    font-size: 11px;
    color: var(--color-muted);
  }

  .stat-val {
    font-size: 11.5px;
    color: var(--color-body);
    text-align: right;
    font-variant-numeric: tabular-nums;
  }

  .token-totals {
    display: flex;
    gap: 18px;
    font-size: 12px;
    color: var(--color-muted);
  }

  .token-totals b {
    color: var(--color-body);
    font-variant-numeric: tabular-nums;
  }

  .bar-row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 3px 8px;
  }

  .bar-label {
    width: 38px;
    flex: none;
    font-size: 10.5px;
    color: var(--color-muted);
    font-variant-numeric: tabular-nums;
  }

  .bar-label.wide {
    width: auto;
    max-width: 160px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .bar-track {
    flex: 1;
    height: 6px;
    border-radius: 3px;
    background: var(--color-surface-elevated);
    overflow: hidden;
  }

  .bar-fill {
    display: block;
    height: 100%;
    background: var(--color-primary);
    opacity: 0.75;
    border-radius: 3px;
  }

  .bar-num {
    font-size: 10.5px;
    color: var(--color-muted);
    font-variant-numeric: tabular-nums;
    min-width: 44px;
    text-align: right;
  }

  .session-name {
    flex: 1;
    min-width: 0;
    font-size: 11.5px;
    color: var(--color-body);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .settings-empty {
    padding: 8px;
    font-size: 11px;
    color: var(--color-dim);
    text-align: center;
  }
</style>
