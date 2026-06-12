<script setup lang="ts">
import { computed } from 'vue'
import { storeToRefs } from 'pinia'

import ImageCanvasViewer from './ImageCanvasViewer.vue'
import SettingsModal from './SettingsModal.vue'
import StatusBar from './StatusBar.vue'
import ThumbnailSidebar from './ThumbnailSidebar.vue'
import TopToolbar from './TopToolbar.vue'
import { useSettingsStore } from '@/stores/settings'

const settingsStore = useSettingsStore()
const { settings } = storeToRefs(settingsStore)

const layoutClasses = computed(() => ({
  'app-layout--compact': settings.value.layout.compactMode,
  'app-layout--thumbnails-bottom': settings.value.layout.thumbnailPosition === 'bottom',
}))
</script>

<template>
  <div class="app-layout" :class="layoutClasses">
    <TopToolbar />
    <main class="app-layout__workspace">
      <ThumbnailSidebar v-if="settings.layout.showThumbnailBar" />
      <ImageCanvasViewer />
    </main>
    <StatusBar v-if="settings.layout.showStatusBar" />
    <SettingsModal />
  </div>
</template>

<style scoped>
.app-layout {
  display: flex;
  min-height: 100vh;
  flex-direction: column;
  overflow: hidden;
  background: var(--app-bg);
  color: var(--text-color);
}

.app-layout__workspace {
  display: flex;
  min-height: 0;
  flex: 1;
}

.app-layout--thumbnails-bottom .app-layout__workspace {
  flex-direction: column-reverse;
}

.app-layout--compact :deep(.top-toolbar) {
  min-height: 48px;
  padding-block: 6px;
}
</style>
