<script setup lang="ts">
import { useI18n } from 'vue-i18n'

const { t } = useI18n()
</script>

<template>
  <footer class="status-bar">
    <span>{{ t('status.fileName') }}: {{ t('placeholder.noImage') }}</span>
    <span>{{ t('status.index') }}: 0 / 0</span>
    <span>{{ t('status.resolution') }}: -</span>
    <span>{{ t('status.zoom') }}: 100%</span>
    <span>{{ t('status.fileSize') }}: -</span>
  </footer>
</template>

<style scoped>
.status-bar {
  display: flex;
  min-height: 30px;
  align-items: center;
  gap: 18px;
  padding: 4px 14px;
  border-top: 1px solid var(--border-color);
  background: var(--panel-bg);
  color: var(--muted-color);
  font-size: 12px;
}

.status-bar span:first-child {
  margin-right: auto;
}
</style>
