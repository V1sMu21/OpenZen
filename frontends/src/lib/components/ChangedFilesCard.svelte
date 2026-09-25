<script lang="ts">
  import { t } from "../i18n";
  import { sidepanel } from "../stores/sidepanel.svelte";
  import { detectArtifactType } from "../utils/artifactType";
  import type { TurnFile } from "../utils/fileChanges";
  import { basename, dirname } from "../utils/paths";

  interface Props {
    /** What this turn produced, deliverables first (see collectTurnFiles). */
    rows: TurnFile[];
    /** The session's working dir — decides which rows can be previewed. */
    workingDir: string;
  }

  let { rows, workingDir }: Props = $props();

  /** Rows shown before the "show the rest" affordance kicks in. */
  const MAX_ROWS = 6;

  let expanded = $state(false);
  let collapsed = $state(false);
  let openError = $state<string | null>(null);

  /** Deliverables are the rows the agent announced with `open_side_panel`. */
  let hasDeliverables = $derived(rows.some((r) => r.label !== undefined));
  /** Tag a row as 交付物 only when the card mixes both kinds: with nothing
   *  else in the list the badge would repeat itself on every row. */
  let showDeliverableTag = $derived(
    hasDeliverables && rows.some((r) => r.label === undefined),
  );
  /** Rows whose line counts are real (a deliverable built by a `code_run`
   *  script has none — see TurnFile.hasStats). */
  let measuredRows = $derived(rows.filter((r) => r.hasStats));
  let totalAdded = $derived(measuredRows.reduce((n, f) => n + f.added, 0));
  let totalRemoved = $derived(measuredRows.reduce((n, f) => n + f.removed, 0));
  let visibleRows = $derived(expanded ? rows : rows.slice(0, MAX_ROWS));
  let hiddenCount = $derived(rows.length - visibleRows.length);

  /** Deliverables change the header copy: 「本次修改 N 个文件」 would understate
   *  a turn whose outcome is an artifact produced by a script rather than by
   *  the file tools. */
  let title = $derived.by(() => {
    const n = rows.length;
    const key = hasDeliverables
      ? n === 1
        ? "changes.producedOne"
        : "changes.produced"
      : n === 1
        ? "changes.titleOne"
        : "changes.title";
    return $t(key).replace("{count}", String(n));
  });

  /** Can the side panel actually show this file?
   *
   *  The agent's file tools fence writes to the working dir OR to a temp root
   *  (crates/oz-tools/src/file_ops.rs:80-92), but `open_artifact`'s whitelist
   *  is the working dir plus registered project roots only
   *  (src-tauri/src/sidepanel/commands.rs:474). So a file the agent
   *  legitimately wrote to /tmp can never be previewed, and offering a button
   *  that always errors would be worse than saying so. */
  function isOpenable(row: TurnFile): boolean {
    const wd = workingDir.replace(/\/+$/, "");
    if (!wd) return false; // working dir not resolved yet — path is relative
    return row.path === wd || row.path.startsWith(wd + "/");
  }

  /** Open the file in the right-hand panel. The panel picks its viewer from
   *  the artifact type, so passing the type yields the same view the tab bar
   *  would have chosen. */
  function open(row: TurnFile) {
    if (!isOpenable(row)) return;
    sidepanel
      .open({
        // The agent's declared type beats the extension map when there is one:
        // it can call a `.json` data file a `spreadsheet`, which the extension
        // map can only render as `code`.
        type: row.type || detectArtifactType(row.path),
        path: row.path,
        label: row.label ?? basename(row.path),
      })
      .then(() => {
        openError = null;
      })
      .catch((err: unknown) => {
        openError = String(err);
      });
  }
</script>

{#snippet fileLabel(row: TurnFile)}
  {#if row.label !== undefined}
    <!-- A deliverable leads with the agent's own title (prose, not a path). -->
    <span class="row-name prose">{row.label}</span>
    <span class="row-dir">{row.display}</span>
  {:else}
    <span class="row-name">{basename(row.display)}</span>
    {#if dirname(row.display)}
      <span class="row-dir">{dirname(row.display)}</span>
    {/if}
  {/if}
{/snippet}

{#if rows.length > 0}
  <div class="changes-card">
    <button
      class="changes-header"
      onclick={() => (collapsed = !collapsed)}
      aria-expanded={!collapsed}
      title={$t("changes.toggle")}
      type="button"
    >
      <svg class="changes-icon" width="13" height="13" viewBox="0 0 14 14" fill="none" aria-hidden="true">
        <path d="M3 1.5h5L11 4.5v7a1 1 0 01-1 1H3a1 1 0 01-1-1v-9a1 1 0 011-1z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/>
        <path d="M8 1.5v3h3" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/>
      </svg>
      <span class="changes-title">{title}</span>
      {#if measuredRows.length > 0}
        <span
          class="changes-stat"
          title={$t("changes.totalTitle")
            .replace("{add}", String(totalAdded))
            .replace("{del}", String(totalRemoved))}
        >
          <span class="stat-add">+{totalAdded}</span>
          <span class="stat-del">−{totalRemoved}</span>
        </span>
      {/if}
      <svg class="changes-chevron" class:rotated={!collapsed} width="10" height="10" viewBox="0 0 10 10" fill="none" aria-hidden="true">
        <path d="M3.5 1.5L6.5 5l-3 3.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/>
      </svg>
    </button>

    {#if !collapsed}
      <ul class="changes-list">
        {#each visibleRows as row (row.path)}
          {@const canOpen = isOpenable(row)}
          <li class="changes-row">
            {#if canOpen}
              <button
                class="row-file"
                onclick={() => open(row)}
                title={row.path}
                type="button"
              >
                {@render fileLabel(row)}
              </button>
            {:else}
              <span class="row-file static" title={row.path}>
                {@render fileLabel(row)}
              </span>
            {/if}
            {#if showDeliverableTag && row.label !== undefined}
              <span class="row-tag" title={$t("changes.deliverableTitle")}>
                {$t("changes.deliverable")}
              </span>
            {/if}
            {#if row.edits > 1}
              <span class="row-edits" title={$t("changes.edits").replace("{n}", String(row.edits))}>
                ×{row.edits}
              </span>
            {/if}
            {#if row.hasStats}
              <span class="row-stat">
                <span class="stat-add">+{row.added}</span>
                <span class="stat-del">−{row.removed}</span>
              </span>
            {:else}
              <!-- Produced (and previewable) but changed by a script, whose
                   line counts never reach the tool args: state that instead of
                   printing a +0 −0 that reads as "nothing changed". -->
              <span class="row-stat unknown" title={$t("changes.statUnknown")}>—</span>
            {/if}
            <button
              class="row-open"
              disabled={!canOpen}
              onclick={() => open(row)}
              title={canOpen
                ? $t("changes.openTitle").replace("{name}", row.display)
                : $t("changes.openUnavailable")}
              type="button"
            >
              {$t("changes.open")}
            </button>
          </li>
        {/each}
      </ul>

      {#if hiddenCount > 0 || expanded}
        <button class="changes-more" onclick={() => (expanded = !expanded)} type="button">
          {expanded
            ? $t("changes.less")
            : $t("changes.more").replace("{n}", String(hiddenCount))}
        </button>
      {/if}

      {#if openError}
        <div class="changes-error" role="alert">
          {$t("changes.openFailed").replace("{msg}", openError)}
        </div>
      {/if}
    {/if}
  </div>
{/if}

<style>
  .changes-card {
    margin: 6px 0 2px;
    border: 1px solid var(--color-hairline);
    border-radius: 8px;
    background: var(--color-surface-soft);
    overflow: hidden;
    font-size: 12px;
  }

  .changes-header {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 7px 10px;
    background: transparent;
    border: none;
    color: var(--color-body);
    cursor: pointer;
    text-align: left;
    font-family: inherit;
    font-size: 12px;
  }
  .changes-header:hover {
    background: var(--color-primary-muted);
  }

  .changes-icon {
    flex: 0 0 auto;
    color: var(--color-muted);
  }
  .changes-title {
    /* Absorbs the row's slack so the diffstat and the chevron stay right-
       aligned whether or not a total is shown. */
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--color-ink);
    font-weight: 500;
  }
  .changes-stat {
    flex: 0 0 auto;
    font-family: var(--font-mono);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    display: inline-flex;
    gap: 6px;
  }
  .stat-add {
    color: var(--color-diff-add);
  }
  .stat-del {
    color: var(--color-diff-del);
  }
  .changes-chevron {
    flex: 0 0 auto;
    color: var(--color-muted);
    transition: transform 0.15s var(--ease-soak);
  }
  .changes-chevron.rotated {
    transform: rotate(90deg);
  }

  .changes-list {
    margin: 0;
    padding: 0;
    list-style: none;
    border-top: 1px solid var(--color-hairline);
  }
  .changes-row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 10px;
  }
  .changes-row + .changes-row {
    border-top: 1px solid var(--color-hairline);
  }
  .changes-row:hover {
    background: var(--color-primary-muted);
  }

  .row-file {
    display: flex;
    align-items: baseline;
    gap: 6px;
    flex: 1;
    min-width: 0;
    padding: 6px 0;
    background: transparent;
    border: none;
    color: inherit;
    cursor: pointer;
    text-align: left;
    font-family: inherit;
    font-size: 12px;
  }
  /* Not previewable: the row keeps showing what changed, but stops
     advertising itself as clickable. */
  .row-file.static {
    cursor: default;
  }
  .row-name {
    flex: 0 0 auto;
    /* A long name must not push the diffstat off the row — clamp it and let
       the directory be the part that yields. */
    max-width: 60%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--font-mono);
    color: var(--color-ink);
  }
  /* A deliverable's label is the agent's prose title, not a path. */
  .row-name.prose {
    font-family: inherit;
  }
  .row-dir {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--color-muted);
  }
  /* Quiet marker: this row is a file the agent announced for preview. */
  .row-tag {
    flex: 0 0 auto;
    padding: 1px 5px;
    border: 1px solid var(--color-hairline-strong);
    border-radius: 3px;
    color: var(--color-muted);
    font-size: 10px;
    line-height: 1.5;
  }
  .row-edits {
    flex: 0 0 auto;
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--color-muted);
  }
  .row-stat {
    flex: 0 0 auto;
    font-family: var(--font-mono);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    display: inline-flex;
    gap: 6px;
  }
  .row-stat.unknown {
    color: var(--color-dim);
  }
  /* DESIGN.md 「Secondary」: transparent, 1px hairline-strong, ink text;
     hover moves the border to azure. */
  .row-open {
    flex: 0 0 auto;
    padding: 2px 8px;
    border: 1px solid var(--color-hairline-strong);
    border-radius: 4px;
    background: transparent;
    color: var(--color-ink);
    cursor: pointer;
    font-family: inherit;
    font-size: 11px;
    transition: border-color 0.15s var(--ease-soak);
  }
  .row-open:hover:not(:disabled) {
    border-color: var(--color-primary);
  }
  .row-open:disabled {
    opacity: 0.45;
    cursor: default;
  }

  .changes-more {
    width: 100%;
    padding: 5px 10px;
    border: none;
    border-top: 1px solid var(--color-hairline);
    background: transparent;
    color: var(--color-muted);
    cursor: pointer;
    font-family: inherit;
    font-size: 11px;
    text-align: left;
  }
  .changes-more:hover {
    color: var(--color-primary);
  }

  .changes-error {
    padding: 5px 10px;
    border-top: 1px solid var(--color-hairline);
    color: var(--color-error);
    font-size: 11px;
  }
</style>
