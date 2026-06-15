<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, watch } from 'vue'
import { message } from 'ant-design-vue'
import { storeToRefs } from 'pinia'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { invoke } from '@tauri-apps/api/core'
import { useI18n } from 'vue-i18n'

import ImageCanvasViewer from './ImageCanvasViewer.vue'
import SettingsModal from './SettingsModal.vue'
import StatusBar from './StatusBar.vue'
import ThumbnailSidebar from './ThumbnailSidebar.vue'
import TopToolbar from './TopToolbar.vue'
import { useAppStore } from '@/stores/app'
import { useDirectoryStore } from '@/stores/directory'
import { useSettingsStore } from '@/stores/settings'
import { useViewerStore } from '@/stores/viewer'
import { convertFileSrc } from '@tauri-apps/api/core'
import { useLargeImage } from '@/composables/useLargeImage'
import { useFileOperations } from '@/composables/useFileOperations'
import { collectNeighborPaths } from '@/utils/neighborPaths'
import { eventToChord, resolveAction, type ActionId } from '@/utils/shortcuts'
import type { ImageEntry } from '@/types/image'

const appStore = useAppStore()
const { t } = useI18n()
const directoryStore = useDirectoryStore()
const settingsStore = useSettingsStore()
const viewerStore = useViewerStore()
const { currentEntry } = storeToRefs(directoryStore)
const { error: directoryError } = storeToRefs(directoryStore)
const { settings } = storeToRefs(settingsStore)
const { closeCurrentLargeImage, openImage } = useLargeImage()
const fileOperations = useFileOperations()
let unlistenOpenPaths: UnlistenFn | null = null
let unlistenDrop: UnlistenFn | null = null

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
  if (currentEntry.value) {
    void openImage(currentEntry.value)
  }
  else {
    closeCurrentLargeImage()
  }
}, { immediate: true })

watch(directoryError, (error) => {
  if (error) void message.error(t('directory.openFailed'))
})

// ─── 预加载去重缓存：模块级 Map，容量 2N+1，随 currentIndex 滑动清理 ──────

/** path → HTMLImageElement，保持引用避免 GC 清除预加载资源。 */
const preloadCache = new Map<string, HTMLImageElement>()

/** RAW 扩展名（对应后端 extended_formats::RAW_EXTENSIONS）：走系统解码，<img> 无法直显。 */
const RAW_PRELOAD_SKIP = new Set(['dng', 'cr2', 'cr3', 'nef', 'arw', 'raf', 'orf', 'rw2'])

/**
 * 判断邻居是否值得做全尺寸 <img> 预热。
 *
 * 仅 <img> 直显路径（jpg/png/小 tiff/heic 等）受益；大图/tile 路径（大 BMP、
 * 超阈值文件、RAW）若也喂给 <img>，asset 协议会在 WebView 主线程整文件读取 +
 * WebKit 解码，造成秒级卡顿。这些一律跳过，预热交给后端 prefetch_system_decode（子线程）。
 */
function shouldImgPreload(entry: ImageEntry, thresholdBytes: number): boolean {
  const dot = entry.name.lastIndexOf('.')
  const ext = dot >= 0 ? entry.name.slice(dot + 1).toLowerCase() : ''
  if (ext === 'bmp') return false                 // 大 BMP 必走 tile；小 BMP 也无需 <img> 预热
  if (RAW_PRELOAD_SKIP.has(ext)) return false      // RAW 走系统解码，<img> 解不了
  if (entry.size >= thresholdBytes) return false   // 超大文件必走大图路径
  return true
}

// ─── M3：切换到新图后预加载前后 N 张原图 ───────────────────────────────────

watch(
  () => currentEntry.value?.path,
  () => {
    // 改为监听 currentEntry.path，切换图片时立即预加载，无需等待 loading 翻转
    const count = settings.value.performance.preloadNormalCount
    const entries = directoryStore.entries
    const idx = directoryStore.currentIndex

    // 邻居大图金字塔由后端子线程预建，不创建 <img>，避免 WebView 主线程解码大图
    const pyramidPaths = collectNeighborPaths(entries, idx, settings.value.largeImage.neighborPrefetchCount)
    if (pyramidPaths.length > 0) {
      void invoke('prefetch_large_pyramid', { paths: pyramidPaths }).catch(() => {})
    }

    if (count <= 0) return

    // 本次需要的邻居（去重，保留 entry 以便按格式/尺寸判定）
    const needed = new Set<string>()
    const neededEntries: ImageEntry[] = []
    for (let i = 1; i <= count; i++) {
      for (const offset of [-i, i]) {
        const target = entries[idx + offset]
        if (target && !needed.has(target.path)) {
          needed.add(target.path)
          neededEntries.push(target)
        }
      }
    }

    // 淘汰不再需要的缓存条目（滑动窗口）
    for (const [path] of preloadCache) {
      if (!needed.has(path)) preloadCache.delete(path)
    }

    // 仅对会走 <img> 直显的邻居创建全尺寸 Image 预热（避免在主线程解码大图）
    const thresholdBytes = settings.value.largeImage.fileSizeThresholdMB * 1024 * 1024
    for (const entry of neededEntries) {
      if (!preloadCache.has(entry.path) && shouldImgPreload(entry, thresholdBytes)) {
        const img = new Image()
        img.src = convertFileSrc(entry.path)
        preloadCache.set(entry.path, img)
      }
    }

    // 系统格式（TIFF/HEIC/RAW…）的预热交给后端子线程；非系统格式后端会自行跳过
    void invoke('prefetch_system_decode', { paths: [...needed] }).catch(() => {})
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
  if (event.code === 'Escape' && viewerStore.isFullscreen) {
    event.preventDefault()
    void toggleFullscreen(false)
    return
  }
  if (event.code === 'Backspace' && (event.metaKey || event.ctrlKey)) {
    event.preventDefault()
    void fileOperations.deleteCurrent()
    return
  }
  if (event.code === 'Space') {
    if (shouldThrottleNavigation(event, 'next')) return
    event.preventDefault()
    directoryStore.selectNext()
    return
  }
  const chord = eventToChord(event)
  if (!chord) return
  const action = resolveAction(chord, settings.value.shortcuts)
  if (!action) return

  // 方向键 auto-repeat 节流（各键独立时间戳，互不干扰）
  if ((action === 'previous' || action === 'next') && shouldThrottleNavigation(event, action)) return

  const actions: Record<ActionId, () => void> = {
    openFile: () => void directoryStore.openImageFile(),
    openDirectory: () => void directoryStore.openDirectory(),
    settings: appStore.openSettings,
    reveal: () => void fileOperations.revealCurrent(),
    copyFile: () => {
      const selection = window.getSelection()
      if (directoryStore.currentEntry && (!selection || selection.isCollapsed)) {
        void fileOperations.copyCurrentFile()
      }
    },
    delete: () => void fileOperations.deleteCurrent(),
    previous: directoryStore.selectPrevious,
    next: directoryStore.selectNext,
    zoomIn: () => viewerStore.zoomIn(settings.value.viewer.zoomStep),
    zoomOut: () => viewerStore.zoomOut(settings.value.viewer.zoomStep),
    fitWindow: () => viewerStore.applyDisplayMode('fit-window'),
    actualSize: () => viewerStore.applyDisplayMode('actual-size'),
    fullscreen: () => void toggleFullscreen(),
    rotateClockwise: fileOperations.rotateClockwise,
    rotateCounterClockwise: fileOperations.rotateCounterClockwise,
  }
  event.preventDefault()
  actions[action]()
}

function shouldThrottleNavigation(event: KeyboardEvent, action: 'previous' | 'next') {
  if (!event.repeat) return false
  const now = Date.now()
  const last = lastRepeatTime[action] ?? 0
  if (now - last < REPEAT_THROTTLE_MS) {
    event.preventDefault()
    return true
  }
  lastRepeatTime[action] = now
  return false
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
  const openPaths = (paths: string[]) => {
    // 单图浏览模式仅取首个路径；文件打开后会扫描其所在目录并补齐同目录列表。
    const path = paths[0]
    if (path) void directoryStore.openExternalPath(path)
  }
  unlistenOpenPaths = await listen<string[]>('open-paths', event => openPaths(event.payload))
  unlistenDrop = await getCurrentWindow().onDragDropEvent(event => {
    if (event.payload.type === 'drop') openPaths(event.payload.paths)
  })
  openPaths(await invoke<string[]>('take_pending_open_paths'))
})
onBeforeUnmount(() => {
  window.removeEventListener('keydown', handleKeydown)
  unlistenOpenPaths?.()
  unlistenDrop?.()
})
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
