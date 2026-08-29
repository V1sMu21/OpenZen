// Settings panel store — Svelte 5 $state rune.
// Tracks only whether the panel is open; section state lives inside
// SettingsPanel so each tab can keep its own load/error lifecycle.

function createSettings() {
  let open = $state(false);

  return {
    get open() {
      return open;
    },
    toggle() {
      open = !open;
    },
    close() {
      open = false;
    },
  };
}

export const settings = createSettings();
