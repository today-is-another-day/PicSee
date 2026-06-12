<script setup lang="ts">
import { useI18n } from 'vue-i18n'

const { t } = useI18n()
</script>

<template>
  <aside class="thumbnail-sidebar">
    <div class="thumbnail-sidebar__heading">{{ t('placeholder.directory') }}</div>
    <a-empty :description="t('placeholder.directoryEmpty')" />
  </aside>
</template>

<style scoped>
.thumbnail-sidebar {
  width: 220px;
  min-height: 0;
  flex: 0 0 220px;
  padding: 14px;
  border-right: 1px solid var(--border-color);
  background: var(--panel-bg);
  overflow: auto;
}

.thumbnail-sidebar__heading {
  margin-bottom: 20px;
  font-weight: 600;
}

:global(.app-layout--thumbnails-bottom) .thumbnail-sidebar {
  width: auto;
  min-height: 138px;
  flex: 0 0 138px;
  border-top: 1px solid var(--border-color);
  border-right: 0;
}
</style>
