// Side Panel state store — Svelte 5 $state rune.
//
// Tauri commands read/write the Rust-side SidePanelState; this store mirrors
// it on the frontend for zero-latency local reads.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export interface Artifact {
  id: string;
  type: string; // "html", "pdf", "code", "spreadsheet", "markdown", "image", "office"
  path: string;
  label: string;
}

export interface SidePanelState {
  visible: boolean;
  width: number;
  artifacts: Artifact[];
  activeId: string | null;
  activeIndex: number;
}

function createSidepanel() {
  const state = $state<SidePanelState>({
    visible: false,
    width: 380,
    artifacts: [],
    activeId: null,
    activeIndex: 0,
  });

  // Track unlisten functions for cleanup
  const unlisteners: (() => void)[] = [];

  // ── Initialize from Rust on mount ──
  async function init() {
    try {
      const rustState = await invoke<SidePanelState>("get_sidepanel_state");
      state.visible = rustState.visible ?? false;
      state.width = rustState.width ?? 380;
      state.artifacts = rustState.artifacts ?? [];
      state.activeId = rustState.activeId ?? null;
      state.activeIndex = state.artifacts.findIndex(
        (a) => a.id === state.activeId,
      );
      if (state.activeIndex < 0) state.activeIndex = 0;
    } catch (_e) {
      // Tauri not available (dev server mode) — stay with defaults
    }
  }

  // ── Listeners ──
  async function setupListeners() {
    try {
      const u1 = await listen<boolean>("sidepanel:toggle", (event) => {
        state.visible = event.payload;
      });
      unlisteners.push(u1);

      const u2 = await listen<Artifact>("sidepanel:artifact-opened", (event) => {
        const a = event.payload;
        const existing = state.artifacts.findIndex((x) => x.id === a.id);
        if (existing >= 0) {
          state.artifacts[existing] = a;
        } else {
          state.artifacts.push(a);
        }
        state.activeId = a.id;
        state.activeIndex = state.artifacts.findIndex((x) => x.id === a.id);
        state.visible = true;
      });
      unlisteners.push(u2);

      const u3 = await listen<{ artifacts: Artifact[]; active_id: string | null }>(
        "sidepanel:artifacts-changed",
        (event) => {
          state.artifacts = event.payload.artifacts ?? [];
          state.activeId = event.payload.active_id ?? null;
          state.activeIndex = state.artifacts.findIndex(
            (a) => a.id === state.activeId,
          );
          if (state.activeIndex < 0) state.activeIndex = 0;
        },
      );
      unlisteners.push(u3);

      const u4 = await listen<number>("sidepanel:width-changed", (event) => {
        state.width = event.payload;
      });
      unlisteners.push(u4);

      const u5 = await listen<string>("sidepanel:tab-switched", (event) => {
        state.activeId = event.payload;
        state.activeIndex = state.artifacts.findIndex(
          (a) => a.id === state.activeId,
        );
        if (state.activeIndex < 0) state.activeIndex = 0;
      });
      unlisteners.push(u5);

      const u6 = await listen("sidepanel:cleared", () => {
        state.artifacts = [];
        state.activeId = null;
        state.activeIndex = 0;
        state.visible = false;
      });
      unlisteners.push(u6);
    } catch (_e) {
      // Tauri not available — no-op
    }
  }

  // ── Commands ──
  async function toggle() {
    state.visible = !state.visible;
    try {
      const confirmed = await invoke<boolean>("toggle_sidepanel");
      state.visible = confirmed;
    } catch {
      state.visible = !state.visible; // rollback
    }
  }

  async function open(artifact: Omit<Artifact, "id">) {
    const result = await invoke<Artifact>("open_artifact", {
      artifactType: artifact.type,
      artifactPath: artifact.path,
      artifactLabel: artifact.label,
    });
    return result;
  }

  async function close() {
    await invoke("close_sidepanel");
  }

  async function setWidth(width: number) {
    await invoke("set_sidepanel_width", { width });
  }

  function prevTab() {
    if (state.artifacts.length === 0) return;
    state.activeIndex =
      (state.activeIndex - 1 + state.artifacts.length) % state.artifacts.length;
    const a = state.artifacts[state.activeIndex];
    state.activeId = a.id;
    invoke("switch_artifact_tab", { artifactId: a.id }).catch(() => {});
  }

  function nextTab() {
    if (state.artifacts.length === 0) return;
    state.activeIndex = (state.activeIndex + 1) % state.artifacts.length;
    const a = state.artifacts[state.activeIndex];
    state.activeId = a.id;
    invoke("switch_artifact_tab", { artifactId: a.id }).catch(() => {});
  }

  async function closeTab(artifactId: string) {
    await invoke("close_artifact_tab", { artifactId });
  }

  function selectTab(artifactId: string) {
    const idx = state.artifacts.findIndex((a) => a.id === artifactId);
    if (idx < 0) return;
    state.activeIndex = idx;
    state.activeId = artifactId;
    invoke("switch_artifact_tab", { artifactId }).catch(() => {});
  }

  async function clearAll() {
    await invoke("clear_sidepanel_artifacts");
  }

  // ── Derived ──
  const activeArtifact = $derived(
    state.artifacts.find((a) => a.id === state.activeId) ?? null,
  );

  const artifactCount = $derived(state.artifacts.length);

  // ── Cleanup ──
  function destroy() {
    for (const u of unlisteners) u();
    unlisteners.length = 0;
  }

  return {
    get visible() { return state.visible; },
    set visible(v: boolean) { state.visible = v; },
    get width() { return state.width; },
    set width(v: number) { state.width = v; },
    get artifacts() { return state.artifacts; },
    get activeId() { return state.activeId; },
    get activeIndex() { return state.activeIndex; },
    get activeArtifact() { return activeArtifact; },
    get artifactCount() { return artifactCount; },
    init,
    setupListeners,
    toggle,
    open,
    close,
    setWidth,
    prevTab,
    nextTab,
    selectTab,
    closeTab,
    clearAll,
    destroy,
  };
}

export const sidepanel = createSidepanel();
