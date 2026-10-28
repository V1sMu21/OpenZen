<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import type { Update } from "@tauri-apps/plugin-updater";
  import { isTauri } from "../api/tauri";
  import { t } from "../i18n";

  /** 更新提示（预览 §08）：
   *
   *  不点即装。图标只打开一张卡：版本 / 体积 / 摘要 / 三个动作。
   *  只有卡里的「立即重启」会中断会话 —— 与项目“绝不擅自重启”的规矩一致。
   *
   *  状态：idle（无更新，零占位）→ available（金点）→ downloading（进度）
   *        → ready（可重启）→ 失败回到 available 并给一行原因。
   */
  type Phase = "idle" | "available" | "downloading" | "ready";

  let phase = $state<Phase>("idle");
  let version = $state("");
  let notes = $state("");
  let date = $state("");
  let totalBytes = $state(0);
  let downloaded = $state(0);
  let error = $state("");
  let open = $state(false);
  let pendingUpdate: Update | null = null;
  let rootEl = $state<HTMLDivElement | undefined>();
  let timer: ReturnType<typeof setInterval> | null = null;

  const pct = $derived(totalBytes > 0 ? Math.min(100, Math.round((downloaded / totalBytes) * 100)) : 0);
  const sizeLabel = $derived(totalBytes > 0 ? `${(totalBytes / 1024 / 1024).toFixed(1)} MB` : "");

  async function checkForUpdate() {
    if (!isTauri() || phase !== "idle") return;
    try {
      const { check } = await import("@tauri-apps/plugin-updater");
      const update = await check();
      if (update) {
        pendingUpdate = update;
        version = update.version;
        notes = (update as unknown as { body?: string }).body ?? "";
        date = (update as unknown as { date?: string }).date ?? "";
        phase = "available";
      }
    } catch {
      // updater endpoints/pubkey missing or offline — stay hidden
    }
  }

  async function download() {
    if (phase !== "available" || !pendingUpdate) return;
    error = "";
    phase = "downloading";
    downloaded = 0;
    try {
      const u = pendingUpdate as unknown as {
        download?: (cb: (e: { event: string; data?: { contentLength?: number; chunkLength?: number } }) => void) => Promise<void>;
      };
      if (typeof u.download === "function") {
        await u.download((e) => {
          if (e.event === "Started") totalBytes = e.data?.contentLength ?? 0;
          if (e.event === "Progress") downloaded += e.data?.chunkLength ?? 0;
        });
      } else {
        await pendingUpdate.downloadAndInstall();
      }
      phase = "ready";
    } catch (e) {
      error = String(e).slice(0, 160);
      phase = "available";
    }
  }

  async function restartNow() {
    if (phase !== "ready" || !pendingUpdate) return;
    try {
      // download() already fetched the payload; install() swaps the bundle.
      await (pendingUpdate as unknown as { install: () => Promise<void> }).install();
      const { relaunch } = await import("@tauri-apps/plugin-process");
      await relaunch();
    } catch (e) {
      error = String(e).slice(0, 160);
    }
  }

  function close() {
    open = false;
  }

  function onWindowClick(e: MouseEvent) {
    if (!open || !rootEl) return;
    if (!rootEl.contains(e.target as Node)) close();
  }

  onMount(() => {
    checkForUpdate();
    // Re-check hourly; the check itself is a cheap manifest fetch.
    timer = setInterval(checkForUpdate, 60 * 60 * 1000);
    window.addEventListener("click", onWindowClick);
    return () => window.removeEventListener("click", onWindowClick);
  });
  onDestroy(() => {
    if (timer) clearInterval(timer);
  });

  const titleText = $derived(
    phase === "available"
      ? $t("update.available").replace("{version}", version)
      : phase === "downloading"
        ? $t("update.downloading")
        : $t("update.installed"),
  );
</script>

{#if phase !== "idle"}
  <div class="update-wrap" bind:this={rootEl}>
    <button
      class="update-btn"
      class:available={phase === "available"}
      class:downloading={phase === "downloading"}
      class:ready={phase === "ready"}
      onclick={() => (open = !open)}
      title={titleText}
      aria-label={titleText}
      aria-expanded={open}
    >
      {#if phase === "ready"}
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M21.8 10A10 10 0 1 1 17 3.34"/>
          <path d="M22 2 12 12"/>
          <path d="M16 2h6v6"/>
        </svg>
      {:else}
        <!-- lucide "download" (ISC) -->
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/>
          <polyline points="7 10 12 15 17 10"/>
          <line x1="12" x2="12" y1="15" y2="3"/>
        </svg>
      {/if}
      {#if phase !== "downloading"}<span class="update-dot" aria-hidden="true"></span>{/if}
    </button>

    {#if open}
      <div class="update-card" role="dialog" aria-label={$t("update.title")}>
        <div class="uc-head">
          <span class="uc-badge" class:ready={phase === "ready"}>
            {#if phase === "ready"}✓{:else if phase === "downloading"}↓{:else}↑{/if}
          </span>
          <span class="uc-title">
            {#if phase === "available"}{$t("update.title")}
            {:else if phase === "downloading"}{$t("update.downloading")}
            {:else}{$t("update.readyTitle")}{/if}
          </span>
          <button class="uc-x" onclick={close} aria-label={$t("update.later")}>✕</button>
        </div>

        <div class="uc-meta">
          <span class="uc-ver">v{version}</span>
          {#if date}<span class="uc-dot">·</span><span>{date.slice(0, 10)}</span>{/if}
          {#if sizeLabel}<span class="uc-dot">·</span><span>{sizeLabel}</span>{/if}
        </div>

        {#if phase === "available"}
          {#if notes}
            <p class="uc-notes">{notes.slice(0, 220)}</p>
          {/if}
          {#if error}<p class="uc-error">{error}</p>{/if}
          <div class="uc-actions">
            <button class="uc-btn" onclick={close}>{$t("update.later")}</button>
            <button class="uc-btn primary" onclick={download}>{$t("update.install")}</button>
          </div>
        {:else if phase === "downloading"}
          <div class="uc-prog"><i style="width:{pct}%"></i></div>
          <div class="uc-prog-meta">
            <span>{sizeLabel ? `${((downloaded / 1048576) || 0).toFixed(1)} MB / ${sizeLabel}` : "…"}</span>
            <span>{pct}%</span>
          </div>
          <div class="uc-actions">
            <button class="uc-btn" onclick={close}>{$t("update.later")}</button>
          </div>
        {:else}
          <p class="uc-notes">{$t("update.readyNote")}</p>
          {#if error}<p class="uc-error">{error}</p>{/if}
          <div class="uc-actions">
            <button class="uc-btn" onclick={close}>{$t("update.later")}</button>
            <button class="uc-btn primary" onclick={restartNow}>{$t("update.restart")}</button>
          </div>
        {/if}
      </div>
    {/if}
  </div>
{/if}

<style>
  .update-wrap {
    position: relative;
    flex: none;
    display: flex;
  }

  /* 金＝“需要你动手”（与授权同语义）；不再是第五种绿。 */
  .update-btn {
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 32px;
    height: 32px;
    padding: 0;
    border-radius: 9px;
    border: 1px solid var(--color-gold-line);
    background: var(--color-gold-soft);
    color: var(--color-gold);
    cursor: pointer;
    transition: background 0.18s, border-color 0.18s;
  }
  .update-btn:hover {
    background: color-mix(in srgb, var(--color-gold) 20%, transparent);
  }
  .update-dot {
    position: absolute;
    top: 4px;
    right: 4px;
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--color-gold);
    box-shadow: 0 0 0 2px var(--color-canvas);
  }
  .update-btn.downloading {
    color: var(--color-dim);
    border-color: var(--color-hairline);
    background: var(--color-surface-soft);
  }
  .update-btn.ready {
    color: var(--color-primary);
    border-color: var(--color-hairline-strong);
    background: var(--color-primary-soft);
  }
  .update-btn.ready .update-dot {
    background: var(--color-primary);
  }

  /* 浮卡：锚在按钮下方，与其余弹层同一套材料 */
  .update-card {
    position: absolute;
    top: calc(100% + 8px);
    right: 0;
    width: 320px;
    z-index: 60;
    background: var(--color-surface-overlay);
    border: 1px solid var(--color-hairline-strong);
    border-radius: 16px;
    box-shadow: 0 30px 80px -20px rgba(0, 0, 0, 0.85);
    padding: 16px;
    animation: soak-in 0.18s var(--ease-soak) both;
  }

  .uc-head {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 8px;
  }
  .uc-badge {
    width: 20px;
    height: 20px;
    border-radius: 7px;
    display: grid;
    place-items: center;
    font-size: 10px;
    color: var(--color-gold);
    background: var(--color-gold-soft);
    border: 1px solid var(--color-gold-line);
  }
  .uc-badge.ready {
    color: var(--color-primary);
    background: var(--color-primary-soft);
    border-color: var(--color-hairline-strong);
  }
  .uc-title {
    font-family: var(--font-serif);
    font-size: 13.5px;
    font-weight: 600;
    color: var(--color-ink);
  }
  .uc-x {
    margin-left: auto;
    border: none;
    background: none;
    color: var(--color-muted);
    cursor: pointer;
    width: 24px;
    height: 24px;
    border-radius: 7px;
  }
  .uc-x:hover {
    background: var(--color-surface-soft);
    color: var(--color-ink);
  }

  .uc-meta {
    display: flex;
    align-items: baseline;
    gap: 6px;
    font-family: var(--font-mono);
    font-size: 9.5px;
    color: var(--color-muted);
    margin-bottom: 10px;
  }
  .uc-ver { color: var(--color-ink); font-size: 10.5px; }
  .uc-dot { opacity: 0.6; }

  .uc-notes {
    margin: 0 0 10px;
    font-size: 11.5px;
    line-height: 1.75;
    color: var(--color-body);
    white-space: pre-wrap;
    max-height: 118px;
    overflow: auto;
  }
  .uc-error {
    margin: 0 0 10px;
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--color-error);
    line-height: 1.5;
  }

  .uc-prog {
    height: 4px;
    border-radius: 999px;
    background: var(--color-hairline);
    overflow: hidden;
    margin: 2px 0 6px;
  }
  .uc-prog i {
    display: block;
    height: 100%;
    border-radius: 999px;
    background: var(--color-primary);
    transition: width 0.2s linear;
  }
  .uc-prog-meta {
    display: flex;
    justify-content: space-between;
    font-family: var(--font-mono);
    font-size: 9.5px;
    color: var(--color-muted);
    margin-bottom: 8px;
  }

  .uc-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 6px;
  }
  .uc-btn {
    height: 30px;
    padding: 0 12px;
    border-radius: 9px;
    border: 1px solid var(--color-hairline);
    background: transparent;
    color: var(--color-dim);
    font-family: var(--font-sans);
    font-size: 11.5px;
    cursor: pointer;
    transition: background 0.16s, color 0.16s, border-color 0.16s;
  }
  .uc-btn:hover {
    background: var(--color-surface-soft);
    color: var(--color-ink);
  }
  .uc-btn.primary {
    background: linear-gradient(180deg, #43a996, #2b7f71);
    border-color: color-mix(in srgb, #66dccb 42%, transparent);
    color: #f0fffb;
  }
  .uc-btn.primary:hover {
    filter: brightness(1.06);
    color: #fff;
  }
</style>
