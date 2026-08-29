<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import type { Update } from "@tauri-apps/plugin-updater";
  import { isTauri } from "../api/tauri";
  import { t } from "../i18n";

  /** Hidden until the updater reports an available release. Any failure
   *  (updater not configured in tauri.conf.json, offline, dev build) keeps
   *  the button out of the DOM — the header looks unchanged. */
  type Phase = "idle" | "available" | "downloading" | "installed";

  let phase = $state<Phase>("idle");
  let version = $state("");
  let pendingUpdate: Update | null = null;
  let timer: ReturnType<typeof setInterval> | null = null;

  async function checkForUpdate() {
    if (!isTauri() || phase !== "idle") return;
    try {
      const { check } = await import("@tauri-apps/plugin-updater");
      const update = await check();
      if (update) {
        pendingUpdate = update;
        version = update.version;
        phase = "available";
      }
    } catch {
      // updater endpoints/pubkey missing or offline — stay hidden
    }
  }

  async function install() {
    if (phase !== "available" || !pendingUpdate) return;
    phase = "downloading";
    try {
      await pendingUpdate.downloadAndInstall();
      phase = "installed";
      const { relaunch } = await import("@tauri-apps/plugin-process");
      setTimeout(() => void relaunch(), 1200);
    } catch {
      phase = "idle"; // install failed — quietly return to hidden
    }
  }

  onMount(() => {
    checkForUpdate();
    // Re-check hourly; the check itself is a cheap manifest fetch.
    timer = setInterval(checkForUpdate, 60 * 60 * 1000);
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
  <button
    class="update-btn"
    class:available={phase === "available"}
    class:downloading={phase === "downloading"}
    class:installed={phase === "installed"}
    onclick={install}
    disabled={phase !== "available"}
    title={titleText}
    aria-label={titleText}
  >
    {#if phase === "installed"}
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M21.8 10A10 10 0 1 1 17 3.34"/>
        <path d="M22 2 12 12"/>
        <path d="M16 2h6v6"/>
      </svg>
    {:else}
      <!-- lucide "download" (ISC) -->
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/>
        <polyline points="7 10 12 15 17 10"/>
        <line x1="12" x2="12" y1="15" y2="3"/>
      </svg>
    {/if}
  </button>
{/if}

<style>
  .update-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    border-radius: 6px;
    border: 1px solid transparent;
    background: none;
    cursor: pointer;
    color: #22c55e;
    animation: updateGlow 1.6s ease-in-out infinite;
    transition: background 0.2s;
  }

  .update-btn.available:hover {
    background: rgba(34, 197, 94, 0.12);
  }

  .update-btn.downloading,
  .update-btn.installed {
    cursor: default;
    opacity: 0.85;
    animation: updateGlow 0.9s ease-in-out infinite;
  }

  @keyframes updateGlow {
    0%,
    100% {
      opacity: 0.65;
    }
    50% {
      opacity: 1;
    }
  }
</style>
