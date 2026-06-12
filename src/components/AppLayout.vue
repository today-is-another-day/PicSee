<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, watch } from 'vue'
import { message } from 'ant-design-vue'
import { storeToRefs } from 'pinia'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { useI18n } from 'vue-i18n'

import ImageCanvasViewer from './ImageCanvasViewer.vue'
import SettingsModal from './SettingsModal.vue'
import StatusBar from './StatusBar.vue'
import ThumbnailSidebar from './ThumbnailSidebar.vue'
import TopToolbar from './TopToolbar.vue'
import { useAppStore } from '@/stores/app'
import { useDirectoryStore } from '@/stores/directory'
import { useImageStore } from '@/stores/image'
import { useSettingsStore } from '@/stores/settings'
import { useViewerStore } from '@/stores/viewer'
import { convertFileSrc } from '@tauri-apps/api/core'

const appStore = useAppStore()
const { t } = useI18n()
const directoryStore = useDirectoryStore()
const imageStore = useImageStore()
const settingsStore = useSettingsStore()
const viewerStore = useViewerStore()
const { currentEntry } = storeToRefs(directoryStore)
const { error: directoryError } = storeToRefs(directoryStore)
const { settings } = storeToRefs(settingsStore)

const layoutClasses = computed(() => ({
  'app-layout--compact': settings.value.layout.compactMode,
  'app-layout--thumbnails-bottom': settings.value.layout.thumbnailPosition === 'bottom',
}))

watch(() => currentEntry.value?.path, (path, previousPath) => {
  if (path !== previousPath && path) {
    const mode = settings.value.viewer.defaultZoomMode
    const isFirstImage = !previousPath
    if (isFirstImage || settings.value.viewer.resetZoomOnSwitch) {
      if (mode === 'remember') {
        if (isFirstImage) viewerStore.resetView('fit-window')
        else viewerStore.preserveView()
      } else {
        viewerStore.resetView(mode)
      }
    } else {
      viewerStore.preserveView()
    }
  }
  imageStore.setCurrent(currentEntry.value)
}, { immediate: true })

watch(directoryError, (error) => {
  if (error) void message.error(t('directory.openFailed'))
})

// ─── M3：当前图加载完成后预加载前后 N 张原图 ──────────────────────────────

watch(
  () => imageStore.loading,
  (loading) => {
    if (loading) return
    // 图片加载完成，预热前后 N 张
    const count = settings.value.performance.preloadNormalCount
    if (count <= 0) return
    const entries = directoryStore.entries
    const idx = directoryStore.currentIndex
    for (let i = 1; i <= count; i++) {
      for (const offset of [-i, i]) {
        const target = entries[idx + offset]
        if (target) {
          const img = new Image()
          img.src = convertFileSrc(target.path)
        }
      }
    }
  },
)

// ─── 方向键 auto-repeat 节流 ───────────────────────────────────────────────

/** 上次通过 repeat 事件切图的时间戳。 */
let lastRepeatTime = 0
const REPEAT_THROTTLE_MS = 80

async function toggleFullscreen(force?: boolean) {
  const next = force ?? !viewerStore.isFullscreen
  try {
    await getCurrentWindow().setFullscreen(next)
    viewerStore.setFullscreen(next)
  } catch (error) {
    console.warn('Unable to change fullscreen state.', error)
  }
}

function handleKeydown(event: KeyboardEvent) {
  if (appStore.settingsVisible || isEditableTarget(event.target)) return
  const command = event.metaKey || event.ctrlKey
  if (command && event.key.toLowerCase() === 'o') {
    event.preventDefault()
    void (event.shiftKey ? directoryStore.openDirectory() : directoryStore.openImageFile())
    return
  }
  if (command && event.key === ',') {
    event.preventDefault()
    appStore.openSettings()
    return
  }
  if (command || event.altKey) return

  // 方向键 auto-repeat 节流
  if (event.repeat && (event.key === 'ArrowLeft' || event.key === 'ArrowRight')) {
    const now = Date.now()
    if (now - lastRepeatTime < REPEAT_THROTTLE_MS) {
      event.preventDefault()
      return
    }
    lastRepeatTime = now
  }

  const actions: Record<string, () => void> = {
    ArrowLeft: directoryStore.selectPrevious,
    ArrowRight: directoryStore.selectNext,
    ' ': directoryStore.selectNext,
    '+': () => viewerStore.zoomIn(settings.value.viewer.zoomStep),
    '=': () => viewerStore.zoomIn(settings.value.viewer.zoomStep),
    '-': () => viewerStore.zoomOut(settings.value.viewer.zoomStep),
    '0': () => viewerStore.applyDisplayMode('fit-window'),
    '1': () => viewerStore.applyDisplayMode('actual-size'),
    f: () => void toggleFullscreen(),
    F: () => void toggleFullscreen(),
    Escape: () => {
      if (viewerStore.isFullscreen) void toggleFullscreen(false)
    },
  }
  const action = actions[event.key]
  if (action) {
    event.preventDefault()
    action()
  }
}

function isEditableTarget(target: EventTarget | null) {
  return target instanceof HTMLElement
    && (target.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName))
}

onMounted(async () => {
  window.addEventListener('keydown', handleKeydown)
  try {
    viewerStore.setFullscreen(await getCurrentWindow().isFullscreen())
  } catch {
    viewerStore.setFullscreen(false)
  }
})
onBeforeUnmount(() => window.removeEventListener('keydown', handleKeydown))
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
