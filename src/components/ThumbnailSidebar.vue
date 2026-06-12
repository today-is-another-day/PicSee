<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { storeToRefs } from 'pinia'
import { convertFileSrc } from '@tauri-apps/api/core'

import { useDirectoryStore } from '@/stores/directory'
import { useSettingsStore } from '@/stores/settings'
import { useThumbnailStore } from '@/stores/thumbnail'

// ─── i18n ──────────────────────────────────────────────────────────────────

const { t } = useI18n()

// ─── stores ────────────────────────────────────────────────────────────────

const directoryStore = useDirectoryStore()
const settingsStore = useSettingsStore()
const thumbnailStore = useThumbnailStore()
const { settings } = storeToRefs(settingsStore)

// ─── 目录名 ────────────────────────────────────────────────────────────────

const directoryName = computed(() => {
  const path = directoryStore.currentPath
  return path?.split(/[\\/]/).filter(Boolean).at(-1) ?? t('placeholder.directory')
})

// ─── 缩略图尺寸 ─────────────────────────────────────────────────────────────

/** 当前设置的缩略图最长边，单位 px，96 / 160 / 256。 */
const thumbSize = computed(() => settings.value.layout.thumbnailSize)

/**
 * 缩略图栏 item 的外框尺寸，略大于缩略图本身（含 padding）。
 * 纵向布局时 item 高度 = thumbSize + 30（文件名行）+ padding
 * 横向布局时 item 宽度 = thumbSize + padding
 */
const ITEM_PADDING = 20 // 上下 padding 合计
const ITEM_LABEL_HEIGHT = 28 // 文件名行高度（固定）

const itemMainSize = computed(() => thumbSize.value + ITEM_PADDING)
const itemFullHeight = computed(() => thumbSize.value + ITEM_PADDING + ITEM_LABEL_HEIGHT)

// ─── 虚拟滚动 ───────────────────────────────────────────────────────────────

/** 容器 DOM 引用。 */
const containerRef = ref<HTMLElement | null>(null)

/** 容器可视区域尺寸（宽/高），横向模式用宽，纵向模式用高。 */
const viewportSize = ref(0)

/** 当前滚动位置（scrollTop 或 scrollLeft）。 */
const scrollPos = ref(0)

/** 是否为横向（底部）布局。 */
const isHorizontal = computed(() => settings.value.layout.thumbnailPosition === 'bottom')

/** 总条目数。 */
const totalCount = computed(() => directoryStore.entries.length)

/** 每个 item 在滚动轴方向上的尺寸。 */
const itemSize = computed(() => isHorizontal.value ? itemMainSize.value : itemFullHeight.value)

/** 总内容尺寸（用于撑开滚动容器）。 */
const totalContentSize = computed(() => totalCount.value * itemSize.value)

/** 可视区域能显示的条目数。 */
const visibleCount = computed(() => Math.ceil(viewportSize.value / itemSize.value))

/** 缓冲区大小（两侧各加 BUFFER_COUNT 个 item）。 */
const BUFFER_COUNT = 6

/** 可视范围起始索引（含 buffer）。 */
const startIndex = computed(() => {
  const raw = Math.floor(scrollPos.value / itemSize.value)
  return Math.max(0, raw - BUFFER_COUNT)
})

/** 可视范围结束索引（含 buffer，不超过总数）。 */
const endIndex = computed(() => {
  const raw = Math.floor(scrollPos.value / itemSize.value) + visibleCount.value
  return Math.min(totalCount.value - 1, raw + BUFFER_COUNT)
})

/** 当前渲染的条目切片。 */
const visibleEntries = computed(() => {
  if (totalCount.value === 0) return []
  return directoryStore.entries.slice(startIndex.value, endIndex.value + 1)
})

/** 虚拟滚动：顶部/左侧占位高度/宽度。 */
const spacerBeforeSize = computed(() => startIndex.value * itemSize.value)
/** 虚拟滚动：底部/右侧占位高度/宽度。 */
const spacerAfterSize = computed(() => (totalCount.value - endIndex.value - 1) * itemSize.value)

// ─── 懒加载 ──────────────────────────────────────────────────────────────────

/** 触发可视范围内的缩略图加载。 */
function requestVisibleThumbnails(): void {
  const size = thumbSize.value
  const concurrency = settings.value.performance.thumbnailConcurrency
  for (const entry of visibleEntries.value) {
    thumbnailStore.request(entry.path, size, concurrency)
  }
}

// ─── 自动滚动到当前图 ──────────────────────────────────────────────────────

/** 滚动容器使当前选中的 item 进入可见区域。 */
function scrollToCurrentItem(): void {
  const container = containerRef.value
  if (!container) return
  const idx = directoryStore.currentIndex
  if (idx < 0) return

  const pos = idx * itemSize.value
  const size = itemSize.value
  const viewStart = isHorizontal.value ? container.scrollLeft : container.scrollTop
  const viewEnd = viewStart + viewportSize.value

  if (pos < viewStart) {
    if (isHorizontal.value) container.scrollLeft = pos
    else container.scrollTop = pos
  } else if (pos + size > viewEnd) {
    const target = pos + size - viewportSize.value
    if (isHorizontal.value) container.scrollLeft = target
    else container.scrollTop = target
  }
}

// ─── 滚动 & resize 处理 ────────────────────────────────────────────────────

function handleScroll(): void {
  const container = containerRef.value
  if (!container) return
  scrollPos.value = isHorizontal.value ? container.scrollLeft : container.scrollTop
}

function updateViewportSize(): void {
  const container = containerRef.value
  if (!container) return
  viewportSize.value = isHorizontal.value ? container.clientWidth : container.clientHeight
}

let resizeObserver: ResizeObserver | null = null

onMounted(() => {
  resizeObserver = new ResizeObserver(() => {
    updateViewportSize()
    requestVisibleThumbnails()
  })
  if (containerRef.value) resizeObserver.observe(containerRef.value)
  updateViewportSize()
  requestVisibleThumbnails()
})

onBeforeUnmount(() => {
  resizeObserver?.disconnect()
})

// ─── 响应式 watch ──────────────────────────────────────────────────────────

// 目录变化时清理旧缓存，重置滚动
watch(
  () => directoryStore.currentPath,
  () => {
    thumbnailStore.evict(new Set(directoryStore.entries.map(e => e.path)))
    scrollPos.value = 0
    if (containerRef.value) {
      if (isHorizontal.value) containerRef.value.scrollLeft = 0
      else containerRef.value.scrollTop = 0
    }
  },
)

// 目录 entries 变化时更新可见缩略图
watch(
  () => directoryStore.entries,
  () => {
    requestVisibleThumbnails()
  },
)

// 滚动位置变化时触发懒加载
watch(scrollPos, () => {
  requestVisibleThumbnails()
})

// 当前图变化时自动滚动
watch(
  () => directoryStore.currentIndex,
  () => {
    scrollToCurrentItem()
  },
)

// 缩略图尺寸变化时清空缓存（需要重新生成不同尺寸的缩略图）
watch(thumbSize, () => {
  thumbnailStore.reset()
  requestVisibleThumbnails()
})

// ─── 工具函数 ──────────────────────────────────────────────────────────────

/** 获取某 path 的缩略图显示 src（loaded 时才有值）。 */
function getThumbnailSrc(path: string): string {
  const state = thumbnailStore.getState(path)
  if (state.status === 'loaded') return state.src
  if (path.toLowerCase().endsWith('.svg')) return convertFileSrc(path)
  return ''
}

/** 获取某 path 的缩略图状态。 */
function getThumbnailStatus(path: string): string {
  return thumbnailStore.getState(path).status
}

/** 获取错误 code 的 i18n 文案。 */
function getErrorI18n(path: string): string {
  const state = thumbnailStore.getState(path)
  if (state.status !== 'error') return ''
  const code = state.code
  const map: Record<string, string> = {
    FILE_TOO_LARGE: t('thumbnail.errorFileTooLarge'),
    IMAGE_TOO_LARGE: t('thumbnail.errorImageTooLarge'),
    UNSUPPORTED_FORMAT: t('thumbnail.errorUnsupportedFormat'),
    DECODE_ERROR: t('thumbnail.errorDecode'),
    NOT_ALLOWED: t('thumbnail.errorNotAllowed'),
    IO_ERROR: t('thumbnail.errorIo'),
  }
  return map[code] ?? t('thumbnail.error')
}
</script>

<template>
  <aside
    ref="containerRef"
    class="thumbnail-sidebar"
    :class="{ 'thumbnail-sidebar--horizontal': isHorizontal }"
    @scroll.passive="handleScroll"
  >
    <!-- 目录标题（纵向模式显示，横向模式空间不足不显示） -->
    <div
      v-if="!isHorizontal"
      class="thumbnail-sidebar__heading"
      :title="directoryStore.currentPath ?? undefined"
    >
      {{ directoryName }}
    </div>

    <!-- 空目录占位 -->
    <a-empty
      v-if="!directoryStore.entries.length"
      :description="t('placeholder.directoryEmpty')"
    />

    <!-- 虚拟滚动容器 -->
    <template v-else>
      <!-- 上方/左侧占位 -->
      <div
        class="thumbnail-sidebar__spacer"
        :style="isHorizontal
          ? { width: spacerBeforeSize + 'px', flex: '0 0 auto' }
          : { height: spacerBeforeSize + 'px' }"
      />

      <!-- 可视区 item -->
      <button
        v-for="(entry, sliceIndex) in visibleEntries"
        :key="entry.path"
        class="thumbnail-sidebar__item"
        :class="{
          'thumbnail-sidebar__item--active':
            startIndex + sliceIndex === directoryStore.currentIndex,
        }"
        :title="entry.name"
        :style="{ '--thumb-size': thumbSize + 'px' }"
        @click="directoryStore.select(startIndex + sliceIndex)"
      >
        <!-- 缩略图区域 -->
        <div class="thumbnail-sidebar__thumb">
          <!-- 已加载 -->
          <img
            v-if="getThumbnailStatus(entry.path) === 'loaded'"
            :src="getThumbnailSrc(entry.path)"
            alt=""
            decoding="async"
            draggable="false"
          />
          <!-- 加载中骨架 -->
          <div
            v-else-if="getThumbnailStatus(entry.path) === 'loading' || getThumbnailStatus(entry.path) === 'idle'"
            class="thumbnail-sidebar__skeleton"
          />
          <!-- 失败占位 -->
          <div
            v-else
            class="thumbnail-sidebar__error-placeholder"
            :title="getErrorI18n(entry.path)"
          >
            <span class="thumbnail-sidebar__error-icon">⚠</span>
          </div>
        </div>

        <!-- 文件名截断 + tooltip -->
        <a-tooltip :title="entry.name" placement="right" :mouse-enter-delay="0.5">
          <span class="thumbnail-sidebar__name">{{ entry.name }}</span>
        </a-tooltip>
      </button>

      <!-- 下方/右侧占位 -->
      <div
        class="thumbnail-sidebar__spacer"
        :style="isHorizontal
          ? { width: spacerAfterSize + 'px', flex: '0 0 auto' }
          : { height: spacerAfterSize + 'px' }"
      />
    </template>
  </aside>
</template>

<style scoped>
/* ─── 容器 ─────────────────────────────────────────────────────────────── */

.thumbnail-sidebar {
  display: flex;
  width: 220px;
  min-height: 0;
  flex: 0 0 220px;
  flex-direction: column;
  padding: 14px 10px;
  border-right: 1px solid var(--border-color);
  background: var(--panel-bg);
  overflow-y: auto;
  overflow-x: hidden;
}

.thumbnail-sidebar--horizontal {
  display: flex;
  width: auto;
  min-height: 0;
  flex: 0 0 auto;
  flex-direction: row;
  align-items: center;
  padding: 8px 10px;
  border-top: 1px solid var(--border-color);
  border-right: 0;
  overflow-x: auto;
  overflow-y: hidden;
}

/* ─── 标题 ────────────────────────────────────────────────────────────── */

.thumbnail-sidebar__heading {
  flex-shrink: 0;
  margin-bottom: 12px;
  overflow: hidden;
  font-weight: 600;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* ─── item ───────────────────────────────────────────────────────────── */

.thumbnail-sidebar__item {
  display: flex;
  flex-shrink: 0;
  align-items: center;
  gap: 8px;
  padding: 6px;
  border: 1px solid transparent;
  border-radius: 8px;
  background: transparent;
  color: inherit;
  cursor: pointer;
  text-align: left;
}

/* 纵向布局：垂直排列，宽度撑满，高度由 --thumb-size 决定 */
.thumbnail-sidebar:not(.thumbnail-sidebar--horizontal) .thumbnail-sidebar__item {
  width: 100%;
  flex-direction: row;
  margin-bottom: 4px;
}

/* 横向布局：水平排列，宽由 --thumb-size 决定 */
.thumbnail-sidebar--horizontal .thumbnail-sidebar__item {
  flex-direction: column;
  width: calc(var(--thumb-size, 96px) + 12px);
  min-height: calc(var(--thumb-size, 96px) + 28px + 12px);
  margin-right: 6px;
}

.thumbnail-sidebar__item:hover,
.thumbnail-sidebar__item--active {
  border-color: #1677ff;
  background: var(--canvas-glow);
}

/* ─── 缩略图容器 ─────────────────────────────────────────────────────── */

.thumbnail-sidebar__thumb {
  display: flex;
  width: var(--thumb-size, 96px);
  height: var(--thumb-size, 96px);
  flex: 0 0 var(--thumb-size, 96px);
  align-items: center;
  justify-content: center;
  overflow: hidden;
  border-radius: 5px;
  background: var(--canvas-bg, #f0f0f0);
}

.thumbnail-sidebar__thumb img {
  width: 100%;
  height: 100%;
  border-radius: 5px;
  object-fit: contain;
}

/* ─── 骨架屏 ─────────────────────────────────────────────────────────── */

.thumbnail-sidebar__skeleton {
  width: 100%;
  height: 100%;
  border-radius: 5px;
  background: linear-gradient(90deg, #e8e8e8 25%, #f5f5f5 50%, #e8e8e8 75%);
  background-size: 200% 100%;
  animation: skeleton-shimmer 1.4s ease infinite;
}

:global(.dark) .thumbnail-sidebar__skeleton {
  background: linear-gradient(90deg, #2a2a2a 25%, #3a3a3a 50%, #2a2a2a 75%);
  background-size: 200% 100%;
}

@keyframes skeleton-shimmer {
  0% { background-position: 200% 0; }
  100% { background-position: -200% 0; }
}

/* ─── 失败占位 ───────────────────────────────────────────────────────── */

.thumbnail-sidebar__error-placeholder {
  display: flex;
  width: 100%;
  height: 100%;
  align-items: center;
  justify-content: center;
  border-radius: 5px;
  background: #fafafa;
  color: #aaa;
}

:global(.dark) .thumbnail-sidebar__error-placeholder {
  background: #1e1e1e;
  color: #555;
}

.thumbnail-sidebar__error-icon {
  font-size: 20px;
}

/* ─── 文件名 ─────────────────────────────────────────────────────────── */

.thumbnail-sidebar__name {
  overflow: hidden;
  font-size: 12px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* 纵向：文件名宽度撑满剩余空间 */
.thumbnail-sidebar:not(.thumbnail-sidebar--horizontal) .thumbnail-sidebar__name {
  flex: 1;
  min-width: 0;
}

/* 横向：文件名撑满 item 宽度 */
.thumbnail-sidebar--horizontal .thumbnail-sidebar__name {
  width: 100%;
  text-align: center;
}

/* ─── 占位 spacer ────────────────────────────────────────────────────── */

.thumbnail-sidebar__spacer {
  flex-shrink: 0;
}
</style>
