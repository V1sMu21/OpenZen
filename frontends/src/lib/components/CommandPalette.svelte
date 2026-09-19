<script lang="ts">
  import { locale } from "../i18n";
  import { filterCommands } from "../utils/slashCommands";

  let lang = $state("zh");
  $effect(() => { lang = $locale; });

  let {
    show = $bindable(false),
    filter = $bindable(""),
    activeIndex = 0,
    onSelect = $bindable((cmd: string) => {}),
  }: {
    show?: boolean;
    filter?: string;
    /** Highlighted row (owned by the input's keyboard navigation). */
    activeIndex?: number;
    onSelect?: (cmd: string) => void;
  } = $props();

  let filtered = $derived(filterCommands(lang, filter));
</script>

{#if show && filtered.length > 0}
  <div class="command-palette">
    {#each filtered as cmd, i (cmd.command)}
      <button
        class="command-item"
        class:active={i === activeIndex}
        onclick={() => onSelect(cmd.command)}
      >
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

  .command-item.active {
    background: var(--color-surface-soft);
    outline: 1px solid var(--color-hairline-strong);
  }
</style>
