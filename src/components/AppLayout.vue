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

// ─── 预加载去重缓存：模块级 Map，容量 2N+1，随 currentIndex 滑动清理 ──────

/** path → HTMLImageElement，保持引用避免 GC 清除预加载资源。 */
const preloadCache = new Map<string, HTMLImageElement>()

// ─── M3：切换到新图后预加载前后 N 张原图 ───────────────────────────────────

watch(
  () => currentEntry.value?.path,
  () => {
    // 改为监听 currentEntry.path，切换图片时立即预加载，无需等待 loading 翻转
    const count = settings.value.performance.preloadNormalCount
    if (count <= 0) return
    const entries = directoryStore.entries
    const idx = directoryStore.currentIndex

    // 本次需要的路径集合
    const needed = new Set<string>()
    for (let i = 1; i <= count; i++) {
      for (const offset of [-i, i]) {
        const target = entries[idx + offset]
        if (target) needed.add(target.path)
      }
    }

    // 淘汰不再需要的缓存条目（滑动窗口）
    for (const [path] of preloadCache) {
      if (!needed.has(path)) preloadCache.delete(path)
    }

    // 对新增路径创建 Image 对象并缓存（避免重复创建）
    for (const path of needed) {
      if (!preloadCache.has(path)) {
        const img = new Image()
        img.src = convertFileSrc(path)
        preloadCache.set(path, img)
      }
    }
  },
)

// ─── 方向键 auto-repeat 节流 ───────────────────────────────────────────────

/** 上次通过 repeat 事件切图的时间戳（按键独立，互不影响）。 */
const lastRepeatTime: Record<string, number> = {}
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

  // 方向键 auto-repeat 节流（各键独立时间戳，互不干扰）
  if (event.repeat && (event.key === 'ArrowLeft' || event.key === 'ArrowRight'
    || event.key === 'ArrowUp' || event.key === 'ArrowDown')) {
    const now = Date.now()
    const last = lastRepeatTime[event.key] ?? 0
    if (now - last < REPEAT_THROTTLE_MS) {
      event.preventDefault()
      return
    }
    lastRepeatTime[event.key] = now
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
