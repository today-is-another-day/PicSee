<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

import { useDirectoryStore } from '@/stores/directory'
import { useImageStore } from '@/stores/image'
import { useViewerStore } from '@/stores/viewer'
import { storeToRefs } from 'pinia'

const { t } = useI18n()
const directoryStore = useDirectoryStore()
const imageStore = useImageStore()
const viewerStore = useViewerStore()
const { loadMode } = storeToRefs(imageStore)
const rawPreview = computed(() => imageStore.largeImageSession?.rawPreview ?? false)
const resolution = computed(() => imageStore.naturalWidth && imageStore.naturalHeight
  ? `${imageStore.naturalWidth} × ${imageStore.naturalHeight}`
  : '-')
const fileSize = computed(() => formatFileSize(imageStore.metadata?.size))
const zoom = computed(() => `${Math.round(viewerStore.zoom * 100)}%`)

function formatFileSize(bytes?: number) {
  if (bytes === undefined) return '-'
  if (bytes < 1024) return `${bytes} B`
  const units = ['KB', 'MB', 'GB', 'TB']
  let value = bytes / 1024
  let unit = units[0]
  for (let index = 1; value >= 1024 && index < units.length; index += 1) {
    value /= 1024
    unit = units[index]
  }
  return `${value.toFixed(value >= 10 ? 1 : 2)} ${unit}`
}
</script>

<template>
  <footer class="status-bar">
    <span :title="imageStore.metadata?.path">{{ t('status.fileName') }}: {{ imageStore.metadata?.name ?? t('placeholder.noImage') }}</span>
    <span>{{ t('status.index') }}: {{ directoryStore.currentIndex + 1 }} / {{ directoryStore.entries.length }}</span>
    <span>{{ t('status.resolution') }}: {{ resolution }}</span>
    <span>{{ t('status.zoom') }}: {{ zoom }}</span>
    <span>{{ t('status.fileSize') }}: {{ fileSize }}</span>
    <span v-if="loadMode && loadMode !== 'normal'" class="status-bar__badge">
      {{ t('status.largeImageMode') }}
    </span>
    <span v-if="rawPreview" class="status-bar__badge">{{ t('status.rawPreviewMode') }}</span>
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

.status-bar__badge {
  padding: 1px 6px;
  border-radius: 4px;
  background: color-mix(in srgb, #1677ff 15%, transparent);
  color: #1677ff;
  font-size: 11px;
}
</style>
