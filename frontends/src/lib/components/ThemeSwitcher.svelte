<script lang="ts">
  let { theme = "dark" as "dark" | "light" | "system" } = $props();

  let resolved = $state<"dark" | "light">(
    typeof window !== "undefined" && localStorage.getItem("ga-theme") === "light" ? "light" : "dark"
  );

  $effect(() => {
    const root = document.documentElement;
    if (resolved === "light") {
      root.classList.add("theme-light");
    } else {
      root.classList.remove("theme-light");
    }
    try { localStorage.setItem("ga-theme", resolved); } catch {}
  });

  function setTheme(t: "dark" | "light" | "system") {
    theme = t;
    if (t === "system") {
      resolved = window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
    } else {
      resolved = t;
    }
  }
</script>

<div class="theme-switcher">
  <button class="theme-btn" class:active={theme === "dark"} onclick={() => setTheme("dark")} title="Dark">
    <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
      <path d="M12.5 9.5a6.5 6.5 0 01-7-7 6.8 6.8 0 00-3 5.7 6.5 6.5 0 0010 1.3z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/>
    </svg>
  </button>
  <button class="theme-btn" class:active={theme === "light"} onclick={() => setTheme("light")} title="Light">
    <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
      <circle cx="7" cy="7" r="3" stroke="currentColor" stroke-width="1.2"/>
      <path d="M7 1v1.5M7 11.5V13M13 7h-1.5M2.5 7H1M11.2 2.8l-1 1M3.8 10.2l-1 1M11.2 11.2l-1-1M3.8 3.8l-1-1" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
    </svg>
  </button>
  <button class="theme-btn" class:active={theme === "system"} onclick={() => setTheme("system")} title="System">
    <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
      <rect x="1.5" y="2.5" width="11" height="7.5" rx="1.5" stroke="currentColor" stroke-width="1.2"/>
      <path d="M4.5 13h5M7 10v3" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
    </svg>
  </button>
</div>

<style>
  .theme-switcher {
    display: flex;
    gap: 2px;
    padding: 3px;
    border-radius: 3px;
    background: var(--color-surface-soft);
    flex-shrink: 0;
  }
  .theme-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    background: none;
    border: none;
    color: var(--color-dim);
    cursor: pointer;
    padding: 3px 5px;
    border-radius: 2px;
    transition: color 0.3s var(--ease-soak, ease), background 0.3s var(--ease-soak, ease);
  }
  .theme-btn:hover {
    color: var(--color-body);
  }
  .theme-btn.active {
    color: var(--color-primary);
    background: var(--color-primary-muted);
  }
</style>
