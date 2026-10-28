<script lang="ts">
  // Plan review panel — the right rail's "approve before it runs" surface.
  //
  // Data comes from the Rust commands (src-tauri/src/plans/mod.rs); this
  // component never reads the JSON file directly, so the decision write path
  // and its event stay in one place.
  import { onMount } from "svelte";
  import type { Artifact } from "../stores/sidepanel.svelte";
  import { decidePlan, getPlan, type Plan } from "../api/plans";
  import { plans } from "../stores/plans.svelte";
  import { renderMarkdown, mathReady } from "../utils/markdown";
  import { t } from "../i18n";

  let { artifact } = $props<{ artifact: Artifact }>();

  let plan = $state<Plan | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(true);
  let submitting = $state(false);
  let chosen = $state<string>("");
  let comment = $state("");

  // The artifact path is `<id>.json`; the command takes the bare id.
  function idFromPath(path: string): string {
    const name = path.split("/").pop() ?? path;
    return name.replace(/\.json$/, "");
  }

  async function load() {
    loading = true;
    error = null;
    try {
      const next = await getPlan(idFromPath(artifact.path));
      plan = next;
      chosen = next.options.length ? next.options[0].label : "";
      comment = "";
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    void load();
  });

  // Re-read when the tab is re-pointed at another plan.
  $effect(() => {
    artifact.path;
    void load();
  });

  let decided = $derived(!!plan?.decision);
  let html = $derived(plan ? renderMarkdown(plan.markdown, { mathVersion: $mathReady }) : "");

  async function decide(kind: "approved" | "rejected") {
    if (!plan || submitting || decided) return;
    submitting = true;
    error = null;
    try {
      plan = await decidePlan(plan.id, kind, chosen, comment.trim());
      await plans.refresh();
    } catch (e) {
      error = String(e);
    } finally {
      submitting = false;
    }
  }
</script>

<div class="plan">
  {#if loading}
    <p class="hint">{$t("plan.loading", "读取方案…")}</p>
  {:else if error && !plan}
    <p class="hint err">{error}</p>
  {:else if plan}
    <header class="phead">
      <span class="kicker" class:pending={!decided} class:ok={plan.status === "approved"} class:bad={plan.status === "rejected"}>
        {#if plan.status === "approved"}{$t("plan.approved", "已批准")}
        {:else if plan.status === "rejected"}{$t("plan.rejected", "已驳回")}
        {:else}{$t("plan.pending", "等待批准")}{/if}
      </span>
      <h2>{plan.title}</h2>
      <p class="meta">
        {#if plan.est_cost}<span>{$t("plan.est", "预计")} {plan.est_cost}</span>{/if}
        {#if plan.est_cost && plan.created_at}<span class="dot">·</span>{/if}
        {#if plan.created_at}<span>{plan.created_at}</span>{/if}
        {#if plan.summary}<span class="dot">·</span><span>{plan.summary}</span>{/if}
      </p>
    </header>

    <div class="pbody">
      {#if plan.markdown}
        <article class="md">{@html html}</article>
      {/if}

      {#if decided && plan.decision}
        <div class="result" class:ok={plan.decision.decision === "approved"}>
          <b>
            {#if plan.decision.decision === "approved"}
              {$t("plan.ran_with", "按所选档位执行")}{plan.decision.option ? `：${plan.decision.option}` : ""}
            {:else}
              {$t("plan.not_run", "这次不执行")}
            {/if}
          </b>
          {#if plan.decision.comment}
            <p class="quote">{plan.decision.comment}</p>
          {/if}
          <p class="stamp">{plan.decision.decided_at}</p>
        </div>
      {:else}
        {#if plan.options.length}
          <div class="options">
            {#each plan.options as option (option.label)}
              <button
                type="button"
                class="option"
                class:on={chosen === option.label}
                onclick={() => (chosen = option.label)}
              >
                <span class="radio"></span>
                <span class="otext">
                  <b>{option.label}</b>
                  {#if option.detail}<span>{option.detail}</span>{/if}
                </span>
              </button>
            {/each}
          </div>
        {/if}

        <label class="lbl" for="plan-comment">{$t("plan.notes", "补充说明")}</label>
        <textarea
          id="plan-comment"
          class="ta"
          bind:value={comment}
          placeholder={$t("plan.notes_ph", "例如：先只改样式，分页下个版本再说")}
        ></textarea>
        <p class="note">
          {$t("plan.reject_note", "驳回时这段话会原样回到 Agent 手里，它据此重写方案。")}
        </p>
      {/if}

      {#if error}<p class="hint err">{error}</p>{/if}
    </div>

    {#if !decided}
      <footer class="pfoot">
        <button class="btn no" disabled={submitting} onclick={() => decide("rejected")}>
          {$t("plan.reject", "驳回")}
        </button>
        <button class="btn ok" disabled={submitting} onclick={() => decide("approved")}>
          {submitting ? $t("plan.working", "处理中…") : $t("plan.approve", "批准并执行")}
        </button>
      </footer>
    {/if}
  {/if}
</div>

<style>
  .plan {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    color: var(--color-ink);
    background: var(--color-surface);
  }
  .hint {
    margin: 18px;
    font-size: 12.5px;
    color: var(--color-dim);
  }
  .hint.err {
    color: var(--color-error);
  }

  .phead {
    flex: none;
    padding: 16px 18px 12px;
    border-bottom: 1px solid var(--color-hairline);
  }
  .kicker {
    display: inline-block;
    font-size: 9.5px;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--color-dim);
    margin-bottom: 8px;
  }
  .kicker.pending {
    color: var(--color-warning);
  }
  .kicker.ok {
    color: var(--color-success);
  }
  .kicker.bad {
    color: var(--color-error);
  }
  .phead h2 {
    margin: 0 0 6px;
    font-family: var(--font-serif, "Songti SC", "Noto Serif SC", serif);
    font-size: 18px;
    line-height: 1.4;
    color: var(--color-ink);
    font-weight: 600;
  }
  .meta {
    margin: 0;
    font-size: 11px;
    color: var(--color-dim);
    line-height: 1.7;
  }
  .dot {
    margin: 0 6px;
  }

  .pbody {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 14px 18px 8px;
  }
  /* Markdown body: the renderer emits plain tags, so style descendants. */
  .md {
    font-size: 13px;
    line-height: 1.75;
    color: var(--color-ink);
  }
  .md :global(h1),
  .md :global(h2),
  .md :global(h3) {
    font-family: var(--font-serif, "Songti SC", serif);
    font-size: 14.5px;
    margin: 16px 0 8px;
    font-weight: 600;
  }
  .md :global(p) {
    margin: 0 0 10px;
  }
  .md :global(ul),
  .md :global(ol) {
    margin: 0 0 10px;
    padding-left: 18px;
  }
  .md :global(li) {
    margin: 4px 0;
  }
  .md :global(ul) {
    list-style: disc;
  }
  .md :global(ol) {
    list-style: decimal;
  }
  .md :global(li)::marker {
    color: var(--color-muted);
  }
  .md :global(code) {
    font-family: var(--font-mono);
    font-size: 11.5px;
    background: var(--color-surface-soft);
    border: 1px solid var(--color-hairline);
    border-radius: 4px;
    padding: 0 4px;
  }
  .md :global(pre) {
    background: var(--color-surface-soft);
    border: 1px solid var(--color-hairline);
    border-radius: 8px;
    padding: 10px 12px;
    overflow: auto;
    margin: 0 0 10px;
  }
  .md :global(table) {
    border-collapse: collapse;
    font-size: 12px;
    margin: 0 0 12px;
  }
  .md :global(th),
  .md :global(td) {
    border: 1px solid var(--color-hairline);
    padding: 5px 8px;
    text-align: left;
  }

  .options {
    margin: 14px 0 4px;
  }
  .option {
    display: flex;
    gap: 10px;
    align-items: flex-start;
    width: 100%;
    text-align: left;
    padding: 10px 12px;
    margin-bottom: 8px;
    border: 1px solid var(--color-hairline);
    border-radius: 10px;
    background: transparent;
    color: inherit;
    font: inherit;
    cursor: pointer;
    transition: border-color 0.16s, background 0.16s;
  }
  .option:hover {
    border-color: color-mix(in srgb, var(--color-primary) 45%, transparent);
  }
  .option.on {
    border-color: var(--color-primary);
    background: var(--color-primary-muted, rgba(147, 195, 214, 0.1));
  }
  .radio {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    border: 1.5px solid var(--color-muted);
    margin-top: 3px;
    flex: none;
  }
  .option.on .radio {
    border-color: var(--color-primary);
    background: var(--color-primary);
    box-shadow: 0 0 0 3px var(--color-primary-muted, rgba(147, 195, 214, 0.14));
  }
  .otext {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .otext b {
    font-size: 12.5px;
    font-weight: 600;
  }
  .otext span {
    font-size: 11px;
    color: var(--color-dim);
    line-height: 1.6;
  }

  .lbl {
    display: block;
    font-size: 9.5px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--color-dim);
    margin: 14px 0 6px;
  }
  .ta {
    width: 100%;
    min-height: 74px;
    resize: vertical;
    border: 1px solid var(--color-hairline);
    border-radius: 9px;
    background: var(--color-surface-soft);
    color: var(--color-ink);
    font: 12px/1.65 inherit;
    padding: 9px 11px;
    outline: none;
  }
  .ta:focus {
    border-color: color-mix(in srgb, var(--color-primary) 55%, transparent);
  }
  .note {
    margin: 8px 0 0;
    font-size: 10px;
    color: var(--color-dim);
    line-height: 1.65;
  }

  .result {
    margin-top: 14px;
    padding: 12px 14px;
    border: 1px solid var(--color-hairline);
    border-left: 2px solid var(--color-error);
    border-radius: 9px;
    background: var(--color-surface-soft);
    font-size: 12.5px;
    line-height: 1.75;
  }
  .result.ok {
    border-left-color: var(--color-success);
  }
  .quote {
    margin: 8px 0 0;
    color: var(--color-ink);
    white-space: pre-wrap;
  }
  .stamp {
    margin: 10px 0 0;
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--color-dim);
  }

  .pfoot {
    flex: none;
    display: flex;
    gap: 10px;
    padding: 12px 18px 16px;
    border-top: 1px solid var(--color-hairline);
  }
  .btn {
    flex: 1;
    height: 36px;
    border-radius: 10px;
    font: inherit;
    font-size: 12.5px;
    cursor: pointer;
    transition: filter 0.16s, border-color 0.16s, color 0.16s;
  }
  .btn:disabled {
    opacity: 0.55;
    cursor: default;
  }
  .btn.no {
    background: transparent;
    border: 1px solid color-mix(in srgb, var(--color-error) 34%, transparent);
    color: var(--color-error);
  }
  .btn.no:hover:not(:disabled) {
    border-color: var(--color-error);
  }
  /* Matches .ask-btn-primary: filled celadon, ink-black label. */
  .btn.ok {
    border: 1px solid var(--color-primary);
    background: var(--color-primary);
    color: #0a0a0b;
    font-weight: 600;
  }
  .btn.ok:hover:not(:disabled) {
    filter: brightness(1.08);
  }
</style>
