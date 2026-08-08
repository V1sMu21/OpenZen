<script lang="ts">
import { showAuthDialog, submitAuthToken } from "../stores/auth";
  import { t, localT } from "../i18n";

let token = $state("");
let error = $state("");

function submit() {
  const trimmed = token.trim();
  if (!trimmed) {
    error = localT("auth.tokenEmpty", "Token cannot be empty");
    return;
  }
  submitAuthToken(trimmed);
  token = "";
  error = "";
}
</script>

{#if $showAuthDialog}
<div class="overlay" role="presentation">
  <div class="dialog" role="dialog" aria-label={$t("auth.required")}>
    <h2>{$t("auth.required")}</h2>
    <p class="desc">{$t("auth.enterToken")}</p>
    <input
      bind:value={token}
      type="text"
      placeholder={$t("auth.placeholder")}
      onkeydown={(e) => e.key === "Enter" && submit()}
      autofocus
    />
    {#if error}
      <p class="error">{error}</p>
    {/if}
    <div class="actions">
      <button class="btn-primary" onclick={submit}>{$t("auth.submit")}</button>
    </div>
  </div>
</div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0,0,0,0.4);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }
  .dialog {
    background: var(--color-canvas, #fff);
    border-radius: 12px;
    padding: 24px;
    width: 400px;
    max-width: 90vw;
    box-shadow: 0 8px 32px rgba(0,0,0,0.2);
  }
  h2 {
    margin: 0 0 8px;
    font-size: 18px;
    font-weight: 600;
    color: var(--color-ink, #111);
  }
  .desc {
    margin: 0 0 16px;
    font-size: 14px;
    color: var(--color-muted, #666);
  }
  input {
    width: 100%;
    padding: 10px 12px;
    border: 1px solid var(--color-hairline, #ddd);
    border-radius: 8px;
    font-size: 14px;
    font-family: var(--font-mono, monospace);
    box-sizing: border-box;
    outline: none;
  }
  input:focus {
    border-color: var(--color-primary, #4a90d9);
  }
  .error {
    color: #c0392b;
    font-size: 13px;
    margin: 8px 0 0;
  }
  .actions {
    margin-top: 16px;
    display: flex;
    justify-content: flex-end;
  }
  .btn-primary {
    padding: 8px 20px;
    background: var(--color-primary, #4a90d9);
    color: white;
    border: none;
    border-radius: 8px;
    font-size: 14px;
    cursor: pointer;
  }
  .btn-primary:hover {
    opacity: 0.9;
  }
</style>
