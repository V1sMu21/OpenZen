<script lang="ts">
  import { t } from "../i18n";

  let {
    filterText = $bindable(""),
  } = $props();

  let inputEl: HTMLInputElement | undefined = $state();

  function clearFilter() {
    filterText = "";
    inputEl?.focus();
  }

  export function focus() {
    inputEl?.focus();
    inputEl?.select();
  }
</script>

<div class="filter-wrap">
  <svg class="filter-icon" width="14" height="14" viewBox="0 0 14 14" fill="none">
    <circle cx="6" cy="6" r="4.5" stroke="currentColor" stroke-width="1.2"/>
    <path d="M9.5 9.5L13 13" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
  </svg>
  <input
    bind:this={inputEl}
    type="text"
    class="filter-input"
    placeholder={$t("sidebar.filterPlaceholder")}
    bind:value={filterText}
  />
  {#if filterText}
    <button class="filter-clear" onclick={clearFilter} aria-label={$t("sidebar.filterClear")}>
      <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
        <path d="M3 3l6 6M9 3l-6 6" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>
      </svg>
    </button>
  {/if}
</div>

<style>
  .filter-wrap {
    display: flex;
    align-items: center;
    gap: 7px;
    margin: 8px 12px 0;
    height: 34px;
    padding: 0 10px;
    border-radius: 10px;
    background: var(--color-surface-soft);
    border: 1px solid var(--color-hairline);
    transition: border-color 0.18s, box-shadow 0.18s;
  }

  .filter-wrap:focus-within {
    border-color: color-mix(in srgb, var(--color-primary) 55%, transparent);
    box-shadow: 0 0 0 3px rgba(111, 216, 196, 0.14);
  }

  .filter-icon {
    flex-shrink: 0;
    color: var(--color-body);
  }

  .filter-input {
    flex: 1;
    border: none;
    background: none;
    outline: none;
    font-size: 12px;
    color: var(--color-ink);
    min-width: 0;
  }

  .filter-input::placeholder {
    color: var(--color-muted);
  }

  .filter-clear {
    flex-shrink: 0;
    width: 20px;
    height: 20px;
    display: flex;
    align-items: center;
    justify-content: center;
    border: none;
    background: none;
    color: var(--color-muted);
    border-radius: 4px;
    cursor: pointer;
    padding: 0;
  }

  .filter-clear:hover {
    background: var(--color-hairline);
    color: var(--color-ink);
  }
</style>
