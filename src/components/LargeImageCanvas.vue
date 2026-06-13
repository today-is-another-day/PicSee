<script setup lang="ts">
/**
 * LargeImageCanvas — 大图 Canvas 渲染组件（M4）。
 *
 * 职责：
 * - 接收 session prop（来自 imageStore.largeImageSession）
 * - 消费 useViewerStore 的 zoom/offset/viewport（只读）
 * - 用 Canvas 2D 渲染 preview（底层）+ tile（上层）
 * - HiDPI：canvas 物理像素 = CSS px × devicePixelRatio
 * - tile 加载：new Image() + picsee:// URL，onload 后写入 LRU，触发重绘
 * - rAF 合批：zoom/offset 变化通过 watch 请求 rAF，不在 event handler 里直接 drawImage
 * - LRU：Map<key, HTMLImageElement>，上限 TILE_CACHE_LIMIT 个
 *
 * 手势层（wheel/drag/双击）由父组件 ImageCanvasViewer 负责，本组件只负责渲染。
 */
import {
  onBeforeUnmount,
  onMounted,
  shallowRef,
  useTemplateRef,
  watch,
} from 'vue'
import { storeToRefs } from 'pinia'
import { useSettingsStore } from '@/stores/settings'
import { useViewerStore } from '@/stores/viewer'
import type { LargeImageSession } from '@/types/image'
import { previewUrl, tileUrl } from '@/utils/largeImageUrl'

// ─── Props ────────────────────────────────────────────────────────
const props = defineProps<{
  session: LargeImageSession
}>()

// ─── Stores ───────────────────────────────────────────────────────
const viewerStore = useViewerStore()
const settingsStore = useSettingsStore()
const { zoom, offset, viewport } = storeToRefs(viewerStore)

// ─── Canvas ref ───────────────────────────────────────────────────
const canvasRef = useTemplateRef<HTMLCanvasElement>('canvas')

// ─── Preview image ────────────────────────────────────────────────
const previewImg = shallowRef<HTMLImageElement | null>(null)
const previewLoaded = shallowRef(false)

// ─── Tile LRU cache ───────────────────────────────────────────────
/** 前端 tile 缓存上限（个）。 */
const TILE_CACHE_LIMIT = 256

/**
 * tile 缓存：key = `${sessionId}:${x}:${y}` → HTMLImageElement。
 * 使用 insertion-order Map 模拟 LRU（每次访问时 delete+set 移到末尾）。
 */
const tileCache = new Map<string, HTMLImageElement>()

/** 正在加载的 tile key 集合，防止重复发起请求。 */
const loadingTiles = new Set<string>()
/** 加载失败的 tile 负缓存，避免每帧重复请求。 */
const failedTiles = new Set<string>()
/** 跟踪在途 Image，卸载或切会话时解除回调。 */
const inflightImages = new Set<HTMLImageElement>()

// ─── rAF 状态 ─────────────────────────────────────────────────────
let rafHandle: number | null = null
let renderScheduled = false
let firstTileReported = false // 防止重复上报首屏 tile 数

// ─── 工具函数 ─────────────────────────────────────────────────────

/**
 * 计算当前 session 的 previewScale：
 * previewMaxSize / max(width, height)
 * zoom 超过此值时 preview 不够清晰，需要 tile 补充。
 */
function computePreviewScale(): number {
  const { width, height, previewMaxSize } = props.session
  return previewMaxSize / Math.max(width, height)
}

/**
 * 由视口和 zoom/offset 计算图像坐标系可见矩形。
 */
function computeVisibleRect() {
  const { width: vpW, height: vpH } = viewport.value
  const z = zoom.value
  const ox = offset.value.x
  const oy = offset.value.y
  const { width: imgW, height: imgH } = props.session

  // canvas 左上角对应的图像坐标（CSS px 坐标系）
  const imgX0 = -ox / z
  const imgY0 = -oy / z
  const imgX1 = (vpW - ox) / z
  const imgY1 = (vpH - oy) / z

  return {
    imgX: Math.max(0, imgX0),
    imgY: Math.max(0, imgY0),
    imgX1: Math.min(imgW, imgX1),
    imgY1: Math.min(imgH, imgY1),
  }
}

/**
 * 由可见矩形计算命中的 tile 网格范围。
 * 返回 { tx0, ty0, tx1, ty1 }（tile 坐标，inclusive）。
 */
function computeVisibleTileRange(imgX: number, imgY: number, imgX1: number, imgY1: number) {
  const { tileSize, width: imgW, height: imgH } = props.session
  const tilesX = Math.ceil(imgW / tileSize)
  const tilesY = Math.ceil(imgH / tileSize)

  return {
    tx0: Math.max(0, Math.floor(imgX / tileSize)),
    ty0: Math.max(0, Math.floor(imgY / tileSize)),
    tx1: Math.min(tilesX - 1, Math.floor((imgX1 - 1) / tileSize)),
    ty1: Math.min(tilesY - 1, Math.floor((imgY1 - 1) / tileSize)),
  }
}

/**
 * 向 Map LRU 末尾移动 key（模拟 access）。
 * 超出上限时删除最旧的条目。
 */
function lruAccess(key: string, img: HTMLImageElement) {
  if (tileCache.has(key)) tileCache.delete(key)
  tileCache.set(key, img)
  if (tileCache.size > TILE_CACHE_LIMIT) {
    const oldest = tileCache.keys().next().value
    if (oldest !== undefined) tileCache.delete(oldest)
  }
}

/**
 * 加载一个 tile。
 * - 已在缓存中 → 直接触发重绘
 * - 正在加载中 → 跳过（onload 回调会触发重绘）
 * - 其他 → 发起 new Image() 请求
 */
function loadTile(tx: number, ty: number) {
  const { sessionId } = props.session
  const key = `${sessionId}:${tx}:${ty}`

  if (tileCache.has(key) || loadingTiles.has(key) || failedTiles.has(key)) return

  loadingTiles.add(key)
  const img = new Image()
  inflightImages.add(img)
  img.src = tileUrl(sessionId, tx, ty)

  img.onload = () => {
    loadingTiles.delete(key)
    inflightImages.delete(img)
    if (props.session.sessionId !== sessionId) return
    lruAccess(key, img)
    scheduleRender()
  }

  img.onerror = () => {
    loadingTiles.delete(key)
    inflightImages.delete(img)
    failedTiles.add(key)
    console.warn(`[PicSee] tile load error: ${key}`)
  }
}

function cancelInflightImages() {
  for (const img of inflightImages) {
    img.onload = null
    img.onerror = null
  }
  inflightImages.clear()
}

/** 请求 rAF 重绘（合批：同一帧内多次调用只触发一次）。 */
function scheduleRender() {
  if (renderScheduled) return
  renderScheduled = true
  rafHandle = requestAnimationFrame(() => {
    renderScheduled = false
    render()
  })
}

/** 核心渲染函数（在 rAF 中调用）。 */
function render() {
  const canvas = canvasRef.value
  if (!canvas) return

  const dpr = window.devicePixelRatio || 1
  const cssW = canvas.clientWidth
  const cssH = canvas.clientHeight

  // 更新 canvas 物理分辨率（HiDPI）
  if (canvas.width !== Math.round(cssW * dpr) || canvas.height !== Math.round(cssH * dpr)) {
    canvas.width = Math.round(cssW * dpr)
    canvas.height = Math.round(cssH * dpr)
  }

  const ctx = canvas.getContext('2d')
  if (!ctx) return

  ctx.clearRect(0, 0, canvas.width, canvas.height)

  const { width: imgW, height: imgH, tileSize } = props.session
  const z = zoom.value
  const ox = offset.value.x
  const oy = offset.value.y

  // ── 底层：preview（拉伸到当前视口对应区域，作为 fallback）──────────
  if (previewLoaded.value && previewImg.value) {
    const canvasX = ox * dpr
    const canvasY = oy * dpr
    const canvasW = imgW * z * dpr
    const canvasH = imgH * z * dpr
    ctx.drawImage(previewImg.value, canvasX, canvasY, canvasW, canvasH)
  }

  // ── 上层：tile（已缓存的直接画，避免闪烁）───────────────────────
  const visRect = computeVisibleRect()
  const { tx0, ty0, tx1, ty1 } = computeVisibleTileRange(
    visRect.imgX, visRect.imgY, visRect.imgX1, visRect.imgY1,
  )
  const visibleTileCount = Math.max(0, tx1 - tx0 + 1) * Math.max(0, ty1 - ty0 + 1)
  const previewScale = computePreviewScale()
  const needTiles = props.session.tileable
    && zoom.value * dpr > previewScale
    // TODO M6：引入 z 级 LOD 后移除此低倍率保护。
    && visibleTileCount <= Math.floor(TILE_CACHE_LIMIT * 0.7)

  if (needTiles) {
    for (let ty = ty0; ty <= ty1; ty++) {
      for (let tx = tx0; tx <= tx1; tx++) {
        const { sessionId } = props.session
        const key = `${sessionId}:${tx}:${ty}`
        const cached = tileCache.get(key)
        if (!cached) continue

        // tile 在图像坐标系中的像素位置
        const tileX = tx * tileSize
        const tileY = ty * tileSize
        const tileW = Math.min(tileSize, imgW - tileX)
        const tileH = Math.min(tileSize, imgH - tileY)

        // 转换到 canvas 物理像素坐标
        const canvasX = (tileX * z + ox) * dpr
        const canvasY = (tileY * z + oy) * dpr
        const canvasW = tileW * z * dpr
        const canvasH = tileH * z * dpr

        ctx.drawImage(cached, canvasX, canvasY, canvasW, canvasH)
      }
    }
  }

  if (needTiles) {
    let firstScreenTileCount = 0
    for (let ty = ty0; ty <= ty1; ty++) {
      for (let tx = tx0; tx <= tx1; tx++) {
        loadTile(tx, ty)
        firstScreenTileCount++
      }
    }
    if (firstScreenTileCount > 0 && !firstTileReported) {
      firstTileReported = true
      const tileMsg = `首屏 tile 数: ${firstScreenTileCount} (tx0=${tx0},ty0=${ty0},tx1=${tx1},ty1=${ty1})`
      if (import.meta.env.DEV) console.log(`[PicSee] ${tileMsg}`)
      // TODO M4-debug：上报首屏 tile 数
      if (import.meta.env.DEV) {
        void fetch('/__picsee_e2e_result', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ event: 'first_screen_tiles', count: firstScreenTileCount, tx0, ty0, tx1, ty1 }),
        }).catch(() => {})
      }
    }

    // ── tile 预取（可见范围外扩 prefetchRadius 圈）──────────────
    const settings = settingsStore.settings.largeImage
    if (settings.enableTilePrefetch && settings.prefetchRadius > 0) {
      const r = settings.prefetchRadius
      const maxTX = Math.ceil(imgW / tileSize) - 1
      const maxTY = Math.ceil(imgH / tileSize) - 1

      for (let ty = Math.max(0, ty0 - r); ty <= Math.min(maxTY, ty1 + r); ty++) {
        for (let tx = Math.max(0, tx0 - r); tx <= Math.min(maxTX, tx1 + r); tx++) {
          // 跳过已在可见范围内的（已在上面加载）
          if (tx >= tx0 && tx <= tx1 && ty >= ty0 && ty <= ty1) continue
          loadTile(tx, ty)
        }
      }
    }
  }
}

// ─── Preview 加载 ─────────────────────────────────────────────────

function loadPreview() {
  const { sessionId } = props.session
  const url = previewUrl(sessionId)
  const img = new Image()
  inflightImages.add(img)
  img.src = url
  img.onload = () => {
    inflightImages.delete(img)
    if (props.session.sessionId !== sessionId) return
    previewImg.value = img
    previewLoaded.value = true
    scheduleRender()
    if (import.meta.env.DEV) console.log(`[PicSee] preview loaded: session=${sessionId}`)
  }
  img.onerror = () => {
    inflightImages.delete(img)
    console.warn(`[PicSee] preview load error: session=${sessionId}`)
  }
}

// ─── 生命周期 & 响应式 ────────────────────────────────────────────

// zoom/offset/viewport 变化时请求重绘（合批）
watch([zoom, offset, viewport], () => {
  scheduleRender()
}, { deep: true })

// session 变化时重新加载 preview，清空 tile 缓存
watch(() => props.session, (newSession, oldSession) => {
  if (newSession.sessionId !== oldSession?.sessionId) {
    cancelInflightImages()
    previewLoaded.value = false
    previewImg.value = null
    tileCache.clear()
    loadingTiles.clear()
    failedTiles.clear()
    firstTileReported = false
    loadPreview()
    scheduleRender()
  }
}, { immediate: false })

let resizeObserver: ResizeObserver | null = null

onMounted(() => {
  loadPreview()
  scheduleRender()

  // ResizeObserver：canvas 容器尺寸变化时重绘
  resizeObserver = new ResizeObserver(() => {
    scheduleRender()
  })
  if (canvasRef.value?.parentElement) {
    resizeObserver.observe(canvasRef.value.parentElement)
  }
})

onBeforeUnmount(() => {
  if (rafHandle !== null) cancelAnimationFrame(rafHandle)
  resizeObserver?.disconnect()
  tileCache.clear()
  loadingTiles.clear()
  failedTiles.clear()
  cancelInflightImages()
})
</script>

<template>
  <canvas
    ref="canvas"
    class="large-image-canvas"
  />
</template>

<style scoped>
.large-image-canvas {
  position: absolute;
  top: 0;
  left: 0;
  width: 100%;
  height: 100%;
  pointer-events: none;
}
</style>
