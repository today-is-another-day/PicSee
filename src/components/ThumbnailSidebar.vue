<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { storeToRefs } from 'pinia'
import { convertFileSrc } from '@tauri-apps/api/core'

import { useDirectoryStore } from '@/stores/directory'
import { useSettingsStore } from '@/stores/settings'
import { useThumbnailStore } from '@/stores/thumbnail'
import { calcItemStep, calcStartIndex, ITEM_PAD, ITEM_BORDER, ITEM_MARGIN, ITEM_STEP_EXTRA } from '@/utils/thumbnailLayout'

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

// ─── 尺寸常量从 thumbnailLayout.ts 导入，JS 与 CSS 共享同一套数值 ──────────
// ITEM_PAD=12, ITEM_BORDER=2, ITEM_MARGIN=4, ITEM_STEP_EXTRA=18
// calcItemStep(thumbSize) = thumbSize + 18
// calcStartIndex(scroll, step, buffer, total) → 起始索引

/** 纵向侧栏参数 */
const SIDEBAR_THUMB_GAP = 8   // thumb 与文件名之间的 gap
const SIDEBAR_SIDE_PAD = 10   // 容器左右 padding 合计
const SIDEBAR_BORDER = 1      // 容器右侧 border

/** 纵向侧栏宽度随 thumbSize 自适应（缩略图 + gap + 文件名最小宽度 + 两侧 padding + border）。 */
const sidebarWidth = computed(() => {
  // 文件名区域最小 80px，让 256 档也能放下
  const nameMinWidth = 80
  return thumbSize.value + ITEM_PAD + SIDEBAR_THUMB_GAP + nameMinWidth + SIDEBAR_SIDE_PAD * 2 + SIDEBAR_BORDER
})

/** 每个 item 在滚动轴方向上的步进。 */
const itemStep = computed(() => calcItemStep(thumbSize.value))

// ─── 虚拟滚动 ───────────────────────────────────────────────────────────────

/** 容器 DOM 引用。 */
const containerRef = ref<HTMLElement | null>(null)

/** 第一个 spacer（spacerBefore）的 DOM 引用，用于测量内容偏移量。 */
const spacerBeforeRef = ref<HTMLElement | null>(null)

/** 容器可视区域尺寸（宽/高），横向模式用宽，纵向模式用高。 */
const viewportSize = ref(0)

/**
 * 内容偏移量：spacer 起始位置在滚动轴方向上距容器滚动原点的距离。
 * 纵向：padding-top(14) + 标题高度 + 标题 margin-bottom(12)
 * 横向：padding-left(10)
 * 通过 spacerBeforeRef.offsetTop / offsetLeft 实测，避免硬编码。
 */
const contentOffset = ref(0)

/** 当前滚动位置（scrollTop 或 scrollLeft）。 */
const scrollPos = ref(0)

/** 是否为横向（底部）布局。 */
const isHorizontal = computed(() => settings.value.layout.thumbnailPosition === 'bottom')

/** 总条目数。 */
const totalCount = computed(() => directoryStore.entries.length)

/** 总内容尺寸（用于撑开滚动容器）。 */
const totalContentSize = computed(() => totalCount.value * itemStep.value)

/** 可视区域能显示的条目数。 */
const visibleCount = computed(() => Math.ceil(viewportSize.value / itemStep.value))

/** 缓冲区大小（两侧各加 BUFFER_COUNT 个 item）。 */
const BUFFER_COUNT = 6

/** 已扣除 contentOffset 的有效滚动量（不小于 0）。 */
const adjustedScroll = computed(() => Math.max(0, scrollPos.value - contentOffset.value))

/** 可视范围起始索引（含 buffer）。 */
const startIndex = computed(() =>
  calcStartIndex(adjustedScroll.value, itemStep.value, BUFFER_COUNT, totalCount.value)
)

/** 可视范围结束索引（含 buffer，不超过总数）。 */
const endIndex = computed(() => {
  const raw = Math.floor(adjustedScroll.value / itemStep.value) + visibleCount.value
  return Math.min(totalCount.value - 1, raw + BUFFER_COUNT)
})

/** 当前渲染的条目切片。 */
const visibleEntries = computed(() => {
  if (totalCount.value === 0) return []
  return directoryStore.entries.slice(startIndex.value, endIndex.value + 1)
})

/** 虚拟滚动：顶部/左侧占位高度/宽度。 */
const spacerBeforeSize = computed(() => startIndex.value * itemStep.value)
/** 虚拟滚动：底部/右侧占位高度/宽度。 */
const spacerAfterSize = computed(() => (totalCount.value - endIndex.value - 1) * itemStep.value)

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

  const step = itemStep.value
  const offset = contentOffset.value
  // item 在滚动内容中的实际位置 = contentOffset + idx * step
  const pos = offset + idx * step
  const viewStart = isHorizontal.value ? container.scrollLeft : container.scrollTop
  const viewEnd = viewStart + viewportSize.value

  if (pos < viewStart) {
    if (isHorizontal.value) container.scrollLeft = pos
    else container.scrollTop = pos
  } else if (pos + step > viewEnd) {
    const target = pos + step - viewportSize.value
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

function updateViewport(): void {
  const container = containerRef.value
  if (!container) return
  viewportSize.value = isHorizontal.value ? container.clientWidth : container.clientHeight
  // 测量 spacer 起始位置，得到内容偏移量
  const spacer = spacerBeforeRef.value
  if (spacer) {
    contentOffset.value = isHorizontal.value ? spacer.offsetLeft : spacer.offsetTop
  }
}

let resizeObserver: ResizeObserver | null = null

onMounted(() => {
  resizeObserver = new ResizeObserver(() => {
    updateViewport()
    requestVisibleThumbnails()
  })
  if (containerRef.value) resizeObserver.observe(containerRef.value)
  updateViewport()
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

// 目录 entries 变化时更新可见缩略图，并在 DOM 更新后重测 contentOffset
watch(
  () => directoryStore.entries,
  () => {
    nextTick(() => {
      updateViewport()
    })
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

// 布局方向切换时重置滚动位置并重读 viewport
watch(isHorizontal, () => {
  scrollPos.value = 0
  if (containerRef.value) {
    containerRef.value.scrollLeft = 0
    containerRef.value.scrollTop = 0
  }
  // nextTick 后 viewport 已更新为新方向尺寸
  requestAnimationFrame(() => {
    updateViewport()
    scrollToCurrentItem()
  })
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
    :style="!isHorizontal ? { width: sidebarWidth + 'px', flex: `0 0 ${sidebarWidth}px` } : {}"
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
        ref="spacerBeforeRef"
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
          >
            <span class="thumbnail-sidebar__error-icon" :title="getErrorI18n(entry.path)">⚠</span>
          </div>
        </div>

        <!-- 文件名截断 + tooltip（纵向右侧，横向顶部） -->
        <a-tooltip
          :title="entry.name"
          :placement="isHorizontal ? 'top' : 'right'"
          :mouse-enter-delay="0.5"
        >
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
  /* 宽度由 JS 根据 thumbSize 动态绑定（sidebarWidth），此处作为 fallback */
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
  padding: 6px 8px;
  border-top: 1px solid var(--border-color);
  border-right: 0;
  overflow-x: auto;
  overflow-y: hidden;
}

/* 横向栏：缩小缩略图与文件名的纵向间距，压低整体栏高 */
.thumbnail-sidebar--horizontal .thumbnail-sidebar__item {
  gap: 4px;
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

/*
 * 尺寸常量对应表（与 script 中 ITEM_PAD/ITEM_BORDER/ITEM_MARGIN/ITEM_STEP_EXTRA 一一对应）：
 *   padding: 6px（上下各 6px）           → ITEM_PAD = 12
 *   border:  1px（上下/左右各 1px）      → ITEM_BORDER = 2
 *   纵向 margin-bottom: 4px              → ITEM_MARGIN = 4
 *   横向 margin-right: 4px + gap 2px = 6px  ≈ ITEM_MARGIN = 4（gap 通过 flex gap 补齐，见下）
 *
 *   纵向 itemStep = thumbSize + 12 + 2 + 4 = thumbSize + 18
 *   横向 itemStep = (thumbSize + 12) + (border 2) + (margin-right 4) = thumbSize + 18
 *
 * 注意：item 高度/宽度由内容撑开（thumb 固定 thumbSize × thumbSize + padding），
 *       JS 直接把 itemStep 作为步进，不依赖浏览器测量。
 */
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
  /* 去掉 outline，选中态用 border 体现 */
  outline: none;
}

/* 纵向布局：垂直排列，宽度撑满，高度由 thumbSize 决定（含 padding/border = thumbSize+14） */
.thumbnail-sidebar:not(.thumbnail-sidebar--horizontal) .thumbnail-sidebar__item {
  width: 100%;
  flex-direction: row;
  margin-bottom: 4px;   /* ITEM_MARGIN = 4，纵向步进的一部分 */
}

/* 横向布局：水平排列，宽由 thumbSize 决定 */
.thumbnail-sidebar--horizontal .thumbnail-sidebar__item {
  flex-direction: column;
  /* 宽度 = thumbSize + padding 6×2 = thumbSize + 12（ITEM_PAD）*/
  width: calc(var(--thumb-size, 96px) + 12px);
  margin-right: 4px;    /* ITEM_MARGIN = 4，横向步进的一部分 */
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
  cursor: default;
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
