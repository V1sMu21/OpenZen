<script lang="ts">
  import { t, locale, tSync } from "../i18n";
  let lang = $state("zh");
  $effect(() => { lang = $locale; });

  interface Command {
    command: string;
    description: string;
    action: () => void;
  }

  let { show = $bindable(false), filter = $bindable(""), onSelect = $bindable((cmd: string) => {}) } = $props();

  const commands = $derived([
    { command: "/help", description: tSync(lang, "cmd.help.desc"), action: () => onSelect("/help") },
    { command: "/clear", description: tSync(lang, "cmd.clear.desc"), action: () => onSelect("/clear") },
    { command: "/new", description: tSync(lang, "cmd.new.desc"), action: () => onSelect("/new") },
    { command: "/model", description: tSync(lang, "cmd.model.desc"), action: () => onSelect("/model") },
    { command: "/sessions", description: tSync(lang, "cmd.sessions.desc"), action: () => onSelect("/sessions") },
    { command: "/export", description: tSync(lang, "cmd.export.desc"), action: () => onSelect("/export") },
    { command: "/compact", description: tSync(lang, "cmd.compact.desc"), action: () => onSelect("/compact") },
    { command: "/resume", description: tSync(lang, "cmd.resume.desc"), action: () => onSelect("/resume") },
  ]);

  let filtered = $derived(
    filter
      ? commands.filter((c) => c.command.includes(filter.toLowerCase()))
      : commands,
  );
</script>

{#if show && filtered.length > 0}
  <div class="command-palette">
    {#each filtered as cmd}
      <button class="command-item" onclick={cmd.action}>
        <span class="cmd-text">{cmd.command}</span>
        <span class="cmd-desc">{cmd.description}</span>
      </button>
    {/each}
  </div>
{/if}

<style>
  .command-palette {
    position: absolute;
    bottom: 100%;
    left: 0;
    right: 0;
    margin-bottom: 4px;
    background: var(--color-surface-overlay);
    border: 1px solid var(--color-hairline-strong);
    border-radius: 8px;
    overflow: hidden;
    z-index: 50;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.3);
  }

  .command-item {
    display: flex;
    align-items: center;
    gap: 12px;
    width: 100%;
    padding: 8px 12px;
    background: none;
    border: none;
    color: var(--color-body);
    cursor: pointer;
    text-align: left;
    transition: background 0.1s;
  }

  .command-item:hover {
    background: var(--color-primary-muted);
    color: var(--color-ink);
  }

  .command-item:not(:last-child) {
    border-bottom: 1px solid var(--color-hairline);
  }

  .cmd-text {
    font-family: var(--font-mono);
    font-size: 13px;
    font-weight: 500;
    color: var(--color-primary);
  }

  .cmd-desc {
    font-size: 12px;
    color: var(--color-muted);
    margin-left: auto;
  }
</style>
