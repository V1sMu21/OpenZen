<script lang="ts">
  import { t } from "../i18n";
</script>

<div class="empty-state">
  <p class="empty-title">{$t("sidepanel.emptyTitle")}</p>
  <p class="empty-hint">
    <!-- i18n-allow: the two quoted strings below are literal UI labels being
         emphasised inside a translated sentence, not untranslated copy. -->
    {@html $t("sidepanel.emptyHint").replace(
      '"Preview in Side Panel"',
      '<strong>"Preview in Side Panel"</strong>'
    ).replace(
      '"在侧边栏中预览"',
      '<strong>"在侧边栏中预览"</strong>'
    )}
  </p>
</div>

<style>
  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100%;
    padding: 24px;
    text-align: center;
    color: var(--color-muted);
  }
  .empty-icon {
    font-size: 40px;
    margin-bottom: 12px;
    opacity: 0.4;
    filter: grayscale(0.3);
  }
  .empty-title {
    font-size: 15px;
    font-weight: 600;
    margin: 0 0 8px;
    color: var(--color-body);
  }
  .empty-hint {
    font-size: 12px;
    line-height: 1.5;
    max-width: 260px;
    opacity: 0.75;
    color: var(--color-muted);
  }
  .empty-hint :global(strong) {
    color: var(--color-primary);
  }
</style>
