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
 * - LRU：Map<key, TileCacheEntry>，按估算解码字节淘汰
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
import { tileUrl } from '@/utils/largeImageUrl'
import { evictByBytes, pickLevel, visibleImageRect } from '@/utils/largeImageCanvas'

// ─── Props ────────────────────────────────────────────────────────
const props = defineProps<{
  session: LargeImageSession
}>()

// ─── Stores ───────────────────────────────────────────────────────
const viewerStore = useViewerStore()
const settingsStore = useSettingsStore()
const { zoom, offset, rotation, viewport } = storeToRefs(viewerStore)

// ─── Canvas ref ───────────────────────────────────────────────────
const canvasRef = useTemplateRef<HTMLCanvasElement>('canvas')

// ─── Preview image ────────────────────────────────────────────────
const previewImg = shallowRef<ImageBitmap | null>(null)
const previewLoaded = shallowRef(false)

// ─── Tile LRU cache ───────────────────────────────────────────────
/** 保留 Phase 1A 的 S1 可见 tile 数安全守卫。 */
const TILE_CACHE_LIMIT = 256
const TILE_CACHE_RATIO = 0.4
const COARSE_TILE_MARGIN = 64

interface TileCacheEntry {
  img: HTMLImageElement
  bytes: number
}

/**
 * tile 缓存：key = `${sessionId}:${z}:${x}:${y}` → TileCacheEntry。
 * 使用 insertion-order Map 模拟 LRU（每次访问时 delete+set 移到末尾）。
 */
const tileCache = new Map<string, TileCacheEntry>()
let tileCacheBytes = 0
let pinnedKeys = new Set<string>()

/** 正在加载的 tile key 集合，防止重复发起请求。 */
const loadingTiles = new Set<string>()
/** 到达重试上限后才永久停止请求。 */
const permanentFailedTiles = new Set<string>()
/** 短期失败的退避状态。 */
const tileRetry = new Map<string, { attempts: number, nextAt: number }>()
/** 确保无交互时，到退避时间也能触发下一次请求。 */
const retryTimers = new Map<string, number>()
/** 跟踪在途 Image，卸载或切会话时解除回调。 */
const inflightImages = new Set<HTMLImageElement>()
const MAX_RETRY = 6

// ─── rAF 状态 ─────────────────────────────────────────────────────
let rafHandle: number | null = null
let renderScheduled = false
let firstTileReported = false // 防止重复上报首屏 tile 数
let selectedLevel: number | null = null

// ─── 工具函数 ─────────────────────────────────────────────────────

/** 按当前物理采样倍率选择完整金字塔层级。 */
function selectLevel(): number {
  const dpr = window.devicePixelRatio || 1
  const effScale = zoom.value * dpr
  return pickLevel(effScale, selectedLevel, props.session.maxLevel)
}

function tileKey(z: number, tx: number, ty: number): string {
  return `${props.session.sessionId}:${z}:${tx}:${ty}`
}

function levelDimensions(level: number) {
  const scale = 2 ** level
  return {
    scale,
    width: Math.ceil(props.session.width / scale),
    height: Math.ceil(props.session.height / scale),
  }
}

/**
 * 由视口和 zoom/offset 计算图像坐标系可见矩形。
 */
function computeVisibleRect() {
  const { width: imgW, height: imgH } = props.session
  return visibleImageRect(viewport.value, zoom.value, offset.value, rotation.value, imgW, imgH)
}

/**
 * 由可见矩形计算命中的 tile 网格范围。
 * 返回 { tx0, ty0, tx1, ty1 }（tile 坐标，inclusive）。
 */
function computeVisibleTileRange(
  level: number,
  imgX: number,
  imgY: number,
  imgX1: number,
  imgY1: number,
) {
  const { tileSize } = props.session
  const { scale, width, height } = levelDimensions(level)
  const tilesX = Math.ceil(width / tileSize)
  const tilesY = Math.ceil(height / tileSize)
  const tileSpan = tileSize * scale

  return {
    tx0: Math.max(0, Math.floor(imgX / tileSpan)),
    ty0: Math.max(0, Math.floor(imgY / tileSpan)),
    tx1: Math.min(tilesX - 1, Math.floor((imgX1 - 1) / tileSpan)),
    ty1: Math.min(tilesY - 1, Math.floor((imgY1 - 1) / tileSpan)),
  }
}

/**
 * 向 Map LRU 末尾移动 key（模拟 access）。
 * 超出字节预算时删除最旧的条目。
 */
function tileCacheLimitBytes(): number {
  const configuredBytes = settingsStore.settings.cache.memoryCacheLimitMB * 1024 * 1024
  const requiredTileCount = Math.floor(TILE_CACHE_LIMIT * 0.7 + COARSE_TILE_MARGIN)
  const requiredBytes = requiredTileCount * props.session.tileSize * props.session.tileSize * 4
  return Math.max(configuredBytes * TILE_CACHE_RATIO, requiredBytes)
}

function estimateTileBytes(img: HTMLImageElement): number {
  const width = img.naturalWidth || props.session.tileSize
  const height = img.naturalHeight || props.session.tileSize
  return width * height * 4
}

function lruAccess(key: string, img: HTMLImageElement) {
  const previous = tileCache.get(key)
  if (previous) {
    tileCacheBytes -= previous.bytes
    tileCache.delete(key)
  }
  const entry = { img, bytes: estimateTileBytes(img) }
  tileCache.set(key, entry)
  tileCacheBytes += entry.bytes
  tileCacheBytes = evictByBytes(tileCache, tileCacheBytes, tileCacheLimitBytes(), pinnedKeys)
}

/**
 * 加载一个 tile。
 * - 已在缓存中 → 直接触发重绘
 * - 正在加载中 → 跳过（onload 回调会触发重绘）
 * - 其他 → 发起 new Image() 请求
 */
function loadTile(z: number, tx: number, ty: number) {
  const { sessionId } = props.session
  const key = tileKey(z, tx, ty)
  const retry = tileRetry.get(key)

  if (
    tileCache.has(key)
    || loadingTiles.has(key)
    || permanentFailedTiles.has(key)
    || (retry && Date.now() < retry.nextAt)
  ) return

  const retryTimer = retryTimers.get(key)
  if (retryTimer !== undefined) window.clearTimeout(retryTimer)
  retryTimers.delete(key)
  loadingTiles.add(key)
  const img = new Image()
  inflightImages.add(img)

  img.onload = () => {
    loadingTiles.delete(key)
    inflightImages.delete(img)
    if (props.session.sessionId !== sessionId) return
    tileRetry.delete(key)
    permanentFailedTiles.delete(key)
    lruAccess(key, img)
    scheduleRender()
  }

  img.onerror = () => {
    loadingTiles.delete(key)
    inflightImages.delete(img)
    if (props.session.sessionId !== sessionId) return
    const attempts = (tileRetry.get(key)?.attempts ?? 0) + 1
    if (attempts >= MAX_RETRY) {
      tileRetry.delete(key)
      permanentFailedTiles.add(key)
      console.warn(`[PicSee] tile load abandoned after ${attempts} attempts: ${key}`)
      return
    }
    const delay = Math.min(2000, 150 * 2 ** (attempts - 1))
    tileRetry.set(key, { attempts, nextAt: Date.now() + delay })
    retryTimers.set(key, window.setTimeout(() => {
      retryTimers.delete(key)
      scheduleRender()
    }, delay))
  }
  img.src = tileUrl(sessionId, z, tx, ty)
}

function cancelInflightImages() {
  for (const img of inflightImages) {
    img.onload = null
    img.onerror = null
  }
  inflightImages.clear()
  loadingTiles.clear()
}

function clearRetryState() {
  for (const timer of retryTimers.values()) window.clearTimeout(timer)
  retryTimers.clear()
  tileRetry.clear()
  permanentFailedTiles.clear()
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
  pinnedKeys = new Set<string>()
  const canvas = canvasRef.value
  if (!canvas) return

  const dpr = window.devicePixelRatio || 1
  const cssW = canvas.clientWidth
  const cssH = canvas.clientHeight

  // HiDPI：zoom=1 仍为 1 图像像素:1 CSS 像素，Canvas 物理尺寸按 DPR 扩展。
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

  const applySourceTransform = () => {
    const s = z * dpr
    if (rotation.value === 90) ctx.setTransform(0, s, -s, 0, (ox + imgH * z) * dpr, oy * dpr)
    else if (rotation.value === 180) ctx.setTransform(-s, 0, 0, -s, (ox + imgW * z) * dpr, (oy + imgH * z) * dpr)
    else if (rotation.value === 270) ctx.setTransform(0, -s, s, 0, ox * dpr, (oy + imgW * z) * dpr)
    else ctx.setTransform(s, 0, 0, s, ox * dpr, oy * dpr)
  }

  const renderTileLevel = (level: number, visRect: ReturnType<typeof computeVisibleRect>) => {
    const { tileSize } = props.session
    const { scale, width, height } = levelDimensions(level)
    const { tx0, ty0, tx1, ty1 } = computeVisibleTileRange(
      level,
      visRect.imgX,
      visRect.imgY,
      visRect.imgX1,
      visRect.imgY1,
    )
    ctx.save()
    applySourceTransform()
    const transform = ctx.getTransform()
    const alignSourceBoundary = (
      coordinate: number,
      xCoefficient: number,
      yCoefficient: number,
    ) => {
      if (xCoefficient !== 0) {
        return (Math.round(transform.e + xCoefficient * coordinate) - transform.e) / xCoefficient
      }
      return (Math.round(transform.f + yCoefficient * coordinate) - transform.f) / yCoefficient
    }
    for (let ty = ty0; ty <= ty1; ty++) {
      for (let tx = tx0; tx <= tx1; tx++) {
        const key = tileKey(level, tx, ty)
        const cached = tileCache.get(key)
        if (!cached) continue
        lruAccess(key, cached.img)
        const levelX = tx * tileSize
        const levelY = ty * tileSize
        const tileW = Math.min(tileSize, width - levelX)
        const tileH = Math.min(tileSize, height - levelY)
        // fix(Task5)：先统一整数原图边界，再经当前旋转变换吸附到共享 device pixel 边界。
        const imageX0 = alignSourceBoundary(Math.round(levelX * scale), transform.a, transform.b)
        const imageY0 = alignSourceBoundary(Math.round(levelY * scale), transform.c, transform.d)
        const imageX1 = alignSourceBoundary(
          Math.round(Math.min(imgW, (levelX + tileW) * scale)),
          transform.a,
          transform.b,
        )
        const imageY1 = alignSourceBoundary(
          Math.round(Math.min(imgH, (levelY + tileH) * scale)),
          transform.c,
          transform.d,
        )
        ctx.drawImage(cached.img, imageX0, imageY0, imageX1 - imageX0, imageY1 - imageY0)
      }
    }
    ctx.restore()
  }

  const pinTileLevel = (level: number, visRect: ReturnType<typeof computeVisibleRect>) => {
    const { tx0, ty0, tx1, ty1 } = computeVisibleTileRange(
      level,
      visRect.imgX,
      visRect.imgY,
      visRect.imgX1,
      visRect.imgY1,
    )
    for (let ty = ty0; ty <= ty1; ty++) {
      for (let tx = tx0; tx <= tx1; tx++) {
        const key = tileKey(level, tx, ty)
        if (tileCache.has(key)) pinnedKeys.add(key)
      }
    }
  }

  // ── 底层：preview（拉伸到当前视口对应区域，作为 fallback）──────────
  if (previewLoaded.value && previewImg.value) {
    ctx.save()
    applySourceTransform()
    ctx.drawImage(previewImg.value, 0, 0, imgW, imgH)
    ctx.restore()
  }

  if (!props.session.tileable) return

  // ── 上层：已缓存粗层兜底，再叠加目标层 ─────────────────────────
  const visRect = computeVisibleRect()
  const targetLevel = selectLevel()
  selectedLevel = targetLevel
  const maxRenderable = props.session.maxLevel

  const { tx0, ty0, tx1, ty1 } = computeVisibleTileRange(
    targetLevel,
    visRect.imgX,
    visRect.imgY,
    visRect.imgX1,
    visRect.imgY1,
  )
  const visibleTileCount = Math.max(0, tx1 - tx0 + 1) * Math.max(0, ty1 - ty0 + 1)
  const exceedsTileBudget = targetLevel === maxRenderable
    && visibleTileCount > Math.floor(TILE_CACHE_LIMIT * 0.7)

  // 绘制前统一 pin 粗层与目标层，避免粗层 LRU 访问淘汰本帧稍后才绘制的目标 tile。
  if (targetLevel < maxRenderable) pinTileLevel(targetLevel + 1, visRect)
  if (!exceedsTileBudget) pinTileLevel(targetLevel, visRect)
  if (targetLevel < maxRenderable) renderTileLevel(targetLevel + 1, visRect)
  if (exceedsTileBudget) return

  renderTileLevel(targetLevel, visRect)

  let firstScreenTileCount = 0
  for (let ty = ty0; ty <= ty1; ty++) {
    for (let tx = tx0; tx <= tx1; tx++) {
      loadTile(targetLevel, tx, ty)
      firstScreenTileCount++
    }
  }
  if (firstScreenTileCount > 0 && !firstTileReported) {
    firstTileReported = true
    const tileMsg = `首屏 tile 数: ${firstScreenTileCount} (z=${targetLevel},tx0=${tx0},ty0=${ty0},tx1=${tx1},ty1=${ty1})`
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
    const level = levelDimensions(targetLevel)
    const maxTX = Math.ceil(level.width / tileSize) - 1
    const maxTY = Math.ceil(level.height / tileSize) - 1

    for (let ty = Math.max(0, ty0 - r); ty <= Math.min(maxTY, ty1 + r); ty++) {
      for (let tx = Math.max(0, tx0 - r); tx <= Math.min(maxTX, tx1 + r); tx++) {
        if (tx >= tx0 && tx <= tx1 && ty >= ty0 && ty <= ty1) continue
        loadTile(targetLevel, tx, ty)
      }
    }
  }
}

// ─── Preview 加载 ─────────────────────────────────────────────────

function loadPreview() {
  const { sessionId, previewW, previewH } = props.session
  void (async () => {
    try {
      const { invoke } = await import('@tauri-apps/api/core')
      const buf = await invoke<ArrayBuffer>('get_preview', { sessionId })
      if (props.session.sessionId !== sessionId) return
      const expected = previewW * previewH * 4
      if (!previewW || !previewH || buf.byteLength < expected) {
        throw new Error(`bad preview: bytes=${buf.byteLength} expected=${expected} ${previewW}×${previewH}`)
      }
      const data = new Uint8ClampedArray(buf, 0, expected)
      const bitmap = await createImageBitmap(new ImageData(data, previewW, previewH))
      if (props.session.sessionId !== sessionId) {
        bitmap.close()
        return
      }
      previewImg.value?.close()
      previewImg.value = bitmap
      previewLoaded.value = true
      scheduleRender()
      if (import.meta.env.DEV) console.log(`[PicSee] preview(raw) loaded: session=${sessionId} ${previewW}×${previewH}`)
    } catch (e) {
      console.warn(`[PicSee] preview load error: session=${sessionId}`, e)
    }
  })()
}

// ─── 生命周期 & 响应式 ────────────────────────────────────────────

// zoom/offset/viewport 变化时请求重绘（合批）
watch([zoom, offset, rotation, viewport], () => {
  scheduleRender()
}, { deep: true })

// session 变化时重新加载 preview，清空 tile 缓存
watch(() => props.session, (newSession, oldSession) => {
  if (newSession.sessionId !== oldSession?.sessionId) {
    cancelInflightImages()
    previewLoaded.value = false
    previewImg.value?.close()
    previewImg.value = null
    tileCache.clear()
    tileCacheBytes = 0
    clearRetryState()
    selectedLevel = null
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
  tileCacheBytes = 0
  clearRetryState()
  cancelInflightImages()
  previewImg.value?.close()
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
