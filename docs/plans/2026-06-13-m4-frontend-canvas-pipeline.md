# M4 前端大图 Canvas 渲染管线实现计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 在前端实现大图 Canvas 渲染管线——probe 分流、preview 显示、tile 按需加载合成渲染、HiDPI 支持、LRU 缓存、tile 预取，并完成验证脚本和端到端日志验证。

**Architecture:** 新增 `useLargeImage.ts` composable 封装大图生命周期（probe→open→close）和 tile 调度；新增 `LargeImageCanvas.vue` 子组件负责 canvas 渲染（消费 viewerStore 的 zoom/offset）；`ImageCanvasViewer.vue` 父组件保留手势层，根据 `imageStore.loadMode` 切换 `<img>` 或 `<LargeImageCanvas>`；`imageStore` 扩展 `loadMode`/session 字段。普通 `<img>` 路径完全保留。

**Tech Stack:** Vue 3 Composition API, TypeScript strict, Pinia, `@tauri-apps/api/core` invoke, Canvas 2D API, requestAnimationFrame, Image() preloading, picsee:// custom protocol

---

## 环境确认（不写代码，只读/查）

### Step E1：确认 tauri.conf.json CSP 已含 picsee:

```bash
grep -o 'picsee:' /Users/wxy/projects/my/projects/PicSee/src-tauri/tauri.conf.json
```

预期输出：`picsee:`（已在 img-src 中）。如未出现，在 img-src 末尾补 `picsee:`。

### Step E2：确认后端 command 已注册

```bash
grep -E 'probe_image|open_large_image|close_large_image' \
  /Users/wxy/projects/my/projects/PicSee/src-tauri/src/lib.rs
```

预期：三个 command 均在 invoke_handler 中。

### Step E3：确认 npm 能构建

```bash
cd /Users/wxy/projects/my/projects/PicSee && npm run build 2>&1 | tail -5
```

预期：构建成功（0 errors）。

---

## Task 1：类型扩展

**Goal:** 在 `src/types/image.ts` 补充大图相关类型；在 `src/stores/image.ts` 扩展 load_mode / session 字段。

**Files:**
- Modify: `src/types/image.ts`
- Modify: `src/stores/image.ts`

### Step 1.1：在 `src/types/image.ts` 末尾追加大图类型

```typescript
// ─── 大图引擎类型（M4）───────────────────────────────────────────

/** probe_image 返回的加载模式。 */
export type LoadMode = 'normal' | 'largeCandidate' | 'tileRequired'

/** probe_image command 返回值。 */
export interface ImageProbe {
  width: number
  height: number
  format: string
  fileSize: number
  isLarge: boolean
  loadMode: LoadMode
}

/** open_large_image command 返回值。 */
export interface OpenLargeImageResult {
  sessionId: number
  generation: number
  width: number
  height: number
  tileSize: number
  previewMaxSize: number
}

/** 前端维护的大图会话状态。 */
export interface LargeImageSession extends OpenLargeImageResult {
  path: string
}
```

### Step 1.2：修改 `src/stores/image.ts`

将 `setCurrent` 替换为带 probe 的异步版本，并增加大图状态字段。
完整替换 `src/stores/image.ts`：

```typescript
import { computed, shallowRef } from 'vue'
import { defineStore } from 'pinia'
import { invoke } from '@tauri-apps/api/core'
import { convertFileSrc } from '@tauri-apps/api/core'

import { useViewerStore } from '@/stores/viewer'
import type { ImageEntry, LargeImageSession, LoadMode } from '@/types/image'

export const useImageStore = defineStore('image', () => {
  const metadata = shallowRef<ImageEntry | null>(null)
  // 普通图片路径（loadMode=normal 时使用）
  const src = shallowRef('')
  const loading = shallowRef(false)
  const error = shallowRef<unknown | null>(null)
  const naturalWidth = shallowRef(0)
  const naturalHeight = shallowRef(0)

  // ─── M4：大图状态 ────────────────────────────────────────────────
  /** 当前图片的加载模式（probe 结果，null 表示未 probe）。 */
  const loadMode = shallowRef<LoadMode | null>(null)
  /** 当前大图会话（仅 loadMode !== 'normal' 时有值）。 */
  const largeImageSession = shallowRef<LargeImageSession | null>(null)

  const hasImage = computed(() => Boolean(metadata.value && (src.value || largeImageSession.value)))

  /**
   * 设置当前图片。
   *
   * loadMode=normal → 走现有 <img> 路径（src 赋值）。
   * largeCandidate/tileRequired → 走大图路径（session 由 useLargeImage composable 填充）。
   *
   * 注意：此函数是同步的，不 probe。probe 由 useLargeImage 在 watch 中执行。
   */
  function setCurrent(entry: ImageEntry | null) {
    useViewerStore().setImageSize(0, 0)
    metadata.value = entry
    // 重置，等待 useLargeImage 的 probe 结果
    loadMode.value = null
    largeImageSession.value = null
    error.value = null

    if (!entry) {
      src.value = ''
      loading.value = false
      return
    }

    // 先设 loading = true；probe 完成后由 useLargeImage 决定走哪条路径
    src.value = ''
    loading.value = true
  }

  /** 普通图片加载成功回调（<img> onload）。 */
  function markLoaded(width: number, height: number) {
    naturalWidth.value = width
    naturalHeight.value = height
    loading.value = false
    error.value = null
  }

  function markError(reason: unknown) {
    loading.value = false
    error.value = reason
  }

  /** 由 useLargeImage 在 probe 完成后调用：确认走普通路径。 */
  function setNormalMode(entry: ImageEntry) {
    loadMode.value = 'normal'
    src.value = convertFileSrc(entry.path)
    // loading 保持 true，等 <img> onload 后由 markLoaded 翻转
  }

  /** 由 useLargeImage 在 open_large_image 完成后调用。 */
  function setLargeImageSession(
    mode: LoadMode,
    session: LargeImageSession,
    width: number,
    height: number,
  ) {
    loadMode.value = mode
    largeImageSession.value = session
    naturalWidth.value = width
    naturalHeight.value = height
    loading.value = false
    error.value = null
    useViewerStore().setImageSize(width, height)
  }

  return {
    metadata,
    src,
    loading,
    error,
    naturalWidth,
    naturalHeight,
    hasImage,
    loadMode,
    largeImageSession,
    setCurrent,
    markLoaded,
    markError,
    setNormalMode,
    setLargeImageSession,
  }
})
```

### Step 1.3：运行 vue-tsc 确认类型无误

```bash
cd /Users/wxy/projects/my/projects/PicSee && npm run build 2>&1 | grep -E 'error|Error' | head -20
```

预期：0 errors（或仅有「useLargeImage not found」，这在后续 Task 创建后消失）。

---

## Task 2：largeImageUrl helper

**Goal:** 新建 `src/utils/largeImageUrl.ts`，封装平台相关 URL 拼接；第一阶段只需 macOS 正确，但留好 Windows/Linux 分支。

**Files:**
- Create: `src/utils/largeImageUrl.ts`

### Step 2.1：创建文件

```typescript
/**
 * 平台相关的大图协议 URL 辅助函数。
 *
 * macOS WKWebView：自定义协议为 picsee://localhost/...
 * Windows/Linux（Tauri 2.x）：自定义协议为 http://picsee.localhost/...
 *
 * 第一阶段（M4）只需 macOS 正确，Windows/Linux 分支已预留，待 M6 验证。
 */

/** 判断是否 macOS（基于 userAgent）。 */
function isMacOS(): boolean {
  return /Mac OS X/i.test(navigator.userAgent)
}

/**
 * 生成 picsee:// 协议的 base URL。
 * macOS: picsee://localhost
 * Windows/Linux: http://picsee.localhost
 */
function picseeBase(): string {
  return isMacOS() ? 'picsee://localhost' : 'http://picsee.localhost'
}

/**
 * 拼接 preview URL。
 * 格式：{base}/preview/{sessionId}/{generation}
 */
export function previewUrl(sessionId: number, generation: number): string {
  return `${picseeBase()}/preview/${sessionId}/${generation}`
}

/**
 * 拼接 tile URL。
 * 格式：{base}/tile/{sessionId}/{generation}/0/{x}/{y}
 * z 固定为 0（原始分辨率；多级 LOD 在后续里程碑实现）。
 */
export function tileUrl(sessionId: number, generation: number, x: number, y: number): string {
  return `${picseeBase()}/tile/${sessionId}/${generation}/0/${x}/${y}`
}
```

---

## Task 3：useLargeImage composable

**Goal:** 新建 `src/composables/useLargeImage.ts`，封装：probe → 分流 → open_large_image → 关闭旧会话；导出 `openImage(entry)` 供 AppLayout watch 调用（替换 `imageStore.setCurrent`）。

**Files:**
- Create: `src/composables/useLargeImage.ts`

### Step 3.1：创建文件

关键设计要点：
1. `openImage(entry)` 先 `imageStore.setCurrent(entry)` 重置状态，再 probe，再根据 loadMode 分流
2. 旧会话的 `close_large_image` fire-and-forget（不 await）
3. 同时只处理最新一次调用（用 token 防止竞态）

```typescript
import { invoke } from '@tauri-apps/api/core'
import { useImageStore } from '@/stores/image'
import { useSettingsStore } from '@/stores/settings'
import type { ImageEntry, ImageProbe, LargeImageSession, OpenLargeImageResult } from '@/types/image'

/** 调用 token，每次 openImage 递增，用于取消旧的 in-flight 请求。 */
let currentToken = 0

/**
 * 大图生命周期 composable。
 *
 * 使用方式：
 * ```ts
 * const { openImage } = useLargeImage()
 * watch(() => entry, (e) => e && openImage(e))
 * ```
 */
export function useLargeImage() {
  const imageStore = useImageStore()
  const settingsStore = useSettingsStore()

  /**
   * 打开图片（含 probe 分流）。
   * loadMode=normal → 走 <img>；largeCandidate/tileRequired → 走大图路径。
   */
  async function openImage(entry: ImageEntry): Promise<void> {
    // 1. 重置 UI 状态，显示 loading spinner
    imageStore.setCurrent(entry)

    // 2. 递增 token，取消旧 in-flight 请求
    const token = ++currentToken
    const oldSession = imageStore.largeImageSession

    // fire-and-forget 关闭旧会话（不阻塞新图加载）
    if (oldSession) {
      void invoke('close_large_image', { sessionId: oldSession.sessionId }).catch(() => {
        // 忽略关闭错误（会话可能已超时）
      })
    }

    try {
      // 3. Probe：只读文件头，极快（<50ms 普通图，<5ms BMP）
      const probeStart = performance.now()
      const probe = await invoke<ImageProbe>('probe_image', { path: entry.path })
      console.log(
        `[PicSee] probe_image: ${entry.name} → loadMode=${probe.loadMode}, ` +
        `${probe.width}×${probe.height}, ${(performance.now() - probeStart).toFixed(0)}ms`
      )

      // token 失效（用户已切换到其他图）→ 丢弃结果
      if (token !== currentToken) return

      const settings = settingsStore.settings.largeImage

      if (probe.loadMode === 'normal') {
        // 4a. 普通路径：让 <img> 加载（imageStore.setNormalMode 赋 src）
        imageStore.setNormalMode(entry)
      } else {
        // 4b. 大图路径（largeCandidate 第一阶段也走大图，保守省内存）
        const openStart = performance.now()
        const result = await invoke<OpenLargeImageResult>('open_large_image', { path: entry.path })

        if (token !== currentToken) {
          // 已切图，关闭刚打开的会话
          void invoke('close_large_image', { sessionId: result.sessionId }).catch(() => {})
          return
        }

        const session: LargeImageSession = { ...result, path: entry.path }
        imageStore.setLargeImageSession(probe.loadMode, session, result.width, result.height)

        console.log(
          `[PicSee] open_large_image: sessionId=${result.sessionId}, gen=${result.generation}, ` +
          `${result.width}×${result.height}, preview 加载耗时=${(performance.now() - openStart).toFixed(0)}ms`
        )
      }
    } catch (err) {
      if (token !== currentToken) return
      console.error('[PicSee] openImage failed:', err)
      imageStore.markError(err)
    }
  }

  return { openImage }
}
```

### Step 3.2：修改 `src/components/AppLayout.vue` 中的 watch

将 AppLayout 中调用 `imageStore.setCurrent(currentEntry.value)` 的地方改为 `openImage`。

找到现有代码：
```typescript
  imageStore.setCurrent(currentEntry.value)
```

替换为：
```typescript
  void openImage(currentEntry.value ?? (() => { imageStore.setCurrent(null); return null })())
```

更简洁地做：在 `AppLayout.vue` 的 `<script setup>` 顶部导入 composable：

```typescript
import { useLargeImage } from '@/composables/useLargeImage'
```

在组件内实例化：
```typescript
const { openImage } = useLargeImage()
```

将 watch 中的：
```typescript
imageStore.setCurrent(currentEntry.value)
```
改为：
```typescript
if (currentEntry.value) {
  void openImage(currentEntry.value)
} else {
  imageStore.setCurrent(null)
}
```

**注意：** 不改变 watch 中关于 viewerStore.resetView/preserveView 的逻辑，保持原有的缩放重置行为。

### Step 3.3：运行构建确认 TS 无误

```bash
cd /Users/wxy/projects/my/projects/PicSee && npm run build 2>&1 | grep -E '^.*error' | head -20
```

预期：0 errors。

---

## Task 4：LargeImageCanvas 子组件（Canvas 渲染核心）

**Goal:** 新建 `src/components/LargeImageCanvas.vue`，接收 `session` prop，实现：
- preview 作为底层（常驻，fallback）
- tile 按视口按需加载，画在 preview 上层
- HiDPI（canvas.width = clientWidth × devicePixelRatio）
- LRU 前端 tile 缓存（Map，上限 256 个）
- rAF 合批重绘（拖拽/缩放不在事件里直接 drawImage）
- tile 预取（可见范围外扩 prefetchRadius 圈）
- generation 校验（onload 时检查 session 是否仍是当前）

**Files:**
- Create: `src/components/LargeImageCanvas.vue`

### Step 4.1：创建 LargeImageCanvas.vue

```vue
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
 * tile 缓存：key = `${sessionId}:${gen}:${x}:${y}` → HTMLImageElement。
 * 使用 insertion-order Map 模拟 LRU（每次访问时 delete+set 移到末尾）。
 */
const tileCache = new Map<string, HTMLImageElement>()

/** 正在加载的 tile key 集合，防止重复发起请求。 */
const loadingTiles = new Set<string>()

// ─── rAF 状态 ─────────────────────────────────────────────────────
let rafHandle: number | null = null
let renderScheduled = false

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
 * 返回图像坐标（像素）：{ imgX, imgY, imgW, imgH }
 */
function computeVisibleRect(dpr: number) {
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
  const { sessionId, generation } = props.session
  const key = `${sessionId}:${generation}:${tx}:${ty}`

  if (tileCache.has(key) || loadingTiles.has(key)) return

  loadingTiles.add(key)
  const img = new Image()
  img.src = tileUrl(sessionId, generation, tx, ty)

  img.onload = () => {
    loadingTiles.delete(key)
    // generation 校验：onload 时确认 session 仍是当前
    if (props.session.sessionId !== sessionId || props.session.generation !== generation) {
      return // 旧会话的 tile，丢弃
    }
    lruAccess(key, img)
    scheduleRender()
  }

  img.onerror = (e) => {
    loadingTiles.delete(key)
    // 410=generation 过期，静默忽略
    // 404/其它 console.warn
    // img.onerror 无法直接拿到 HTTP status，picsee:// 下统一 warn
    console.warn(`[PicSee] tile load error: ${key}`, e)
  }
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

  // 更新 canvas 物理分辨率（HiDPI B9）
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
    // 图像在 canvas 中的绘制位置（物理像素）
    const canvasX = ox * dpr
    const canvasY = oy * dpr
    const canvasW = imgW * z * dpr
    const canvasH = imgH * z * dpr
    ctx.drawImage(previewImg.value, canvasX, canvasY, canvasW, canvasH)
  }

  // ── 上层：tile（仅在 zoom 超过 previewScale 时 tile 才有意义，
  //           但即使 zoom 低，也画已加载的 tile 以避免闪烁）─────────
  const visRect = computeVisibleRect(dpr)
  const { tx0, ty0, tx1, ty1 } = computeVisibleTileRange(
    visRect.imgX, visRect.imgY, visRect.imgX1, visRect.imgY1
  )

  for (let ty = ty0; ty <= ty1; ty++) {
    for (let tx = tx0; tx <= tx1; tx++) {
      const { sessionId, generation } = props.session
      const key = `${sessionId}:${generation}:${tx}:${ty}`
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

  // ── tile 按需加载（可见范围）────────────────────────────────────
  const previewScale = computePreviewScale()
  const needTiles = zoom.value > previewScale

  if (needTiles) {
    for (let ty = ty0; ty <= ty1; ty++) {
      for (let tx = tx0; tx <= tx1; tx++) {
        loadTile(tx, ty)
      }
    }

    // ── tile 预取（可见范围外扩 prefetchRadius 圈）──────────────
    const settings = settingsStore.settings.largeImage
    if (settings.enableTilePrefetch && settings.prefetchRadius > 0) {
      const r = settings.prefetchRadius
      const { tileSize: ts, width: w, height: h } = props.session
      const maxTX = Math.ceil(w / ts) - 1
      const maxTY = Math.ceil(h / ts) - 1

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
  const { sessionId, generation } = props.session
  const img = new Image()
  img.src = previewUrl(sessionId, generation)
  img.onload = () => {
    if (props.session.sessionId !== sessionId || props.session.generation !== generation) return
    previewImg.value = img
    previewLoaded.value = true
    scheduleRender()
  }
  img.onerror = () => {
    console.warn(`[PicSee] preview load error: session=${sessionId}, gen=${generation}`)
  }
}

// ─── 生命周期 & 响应式 ────────────────────────────────────────────

// zoom/offset/viewport 变化时请求重绘（合批）
watch([zoom, offset, viewport], () => {
  scheduleRender()
}, { deep: true })

// session 变化时重新加载 preview，清空 tile 缓存
watch(() => props.session, (newSession, oldSession) => {
  if (newSession.sessionId !== oldSession?.sessionId || newSession.generation !== oldSession?.generation) {
    previewLoaded.value = false
    previewImg.value = null
    tileCache.clear()
    loadingTiles.clear()
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
```

---

## Task 5：改造 ImageCanvasViewer.vue（双路径切换）

**Goal:** 在 `ImageCanvasViewer.vue` 中根据 `imageStore.loadMode` 切换 `<img>`（普通路径）或 `<LargeImageCanvas>`（大图路径），手势层逻辑完全保留，普通图不回归。

**Files:**
- Modify: `src/components/ImageCanvasViewer.vue`

### Step 5.1：修改 ImageCanvasViewer.vue

在 `<script setup>` 顶部新增导入：

```typescript
import LargeImageCanvas from '@/components/LargeImageCanvas.vue'
```

在 storeToRefs 的解构中增加新字段：

```typescript
const { error, hasImage, loading, src, loadMode, largeImageSession } = storeToRefs(imageStore)
```

在 `<template>` 中将原有 `<img>` 部分替换为双路径条件渲染：

**原来的 template 结构：**
```vue
<img
  v-if="hasImage"
  class="image-viewer__image"
  :src="src"
  :style="imageStyle"
  :alt="imageStore.metadata?.name"
  draggable="false"
  @load="handleLoad"
  @error="handleError"
>
<a-spin v-if="loading" class="image-viewer__state" size="large" />
```

**替换为：**
```vue
<!-- 普通图片路径（loadMode=normal 或 null） -->
<img
  v-if="hasImage && loadMode === 'normal' && src"
  class="image-viewer__image"
  :src="src"
  :style="imageStyle"
  :alt="imageStore.metadata?.name"
  draggable="false"
  @load="handleLoad"
  @error="handleError"
>
<!-- 大图路径：canvas 渲染 -->
<LargeImageCanvas
  v-else-if="hasImage && largeImageSession"
  :session="largeImageSession"
/>
<!-- loading 状态：probe 或 open_large_image 期间 -->
<a-spin v-if="loading" class="image-viewer__state" size="large" />
```

**注意：** `imageStyle` 计算属性仍然只用于 `<img>` 路径，canvas 路径通过 viewerStore 的状态由 LargeImageCanvas 内部绘制，不需要 CSS transform。

### Step 5.2：验证编译

```bash
cd /Users/wxy/projects/my/projects/PicSee && npm run build 2>&1 | grep -E 'error TS' | head -20
```

预期：0 errors。

---

## Task 6：StatusBar 大图模式标识 + i18n

**Goal:** 大图打开时状态栏显示「大图模式」标识；补全 zh-CN / en-US 新文案。

**Files:**
- Modify: `src/locales/zh-CN.ts`
- Modify: `src/locales/en-US.ts`
- Modify: `src/components/StatusBar.vue`

### Step 6.1：补充 zh-CN.ts

在 `status` 对象中追加：
```typescript
largeImageMode: '大图模式',
```

（找到 `status: { fileName: '文件名', ...` 在末尾属性后追加）

### Step 6.2：补充 en-US.ts

在 `status` 对象中追加：
```typescript
largeImageMode: 'Large image mode',
```

### Step 6.3：修改 StatusBar.vue

在 `<script setup>` 中增加 loadMode：
```typescript
const { naturalWidth, naturalHeight, metadata, loadMode } = storeToRefs(imageStore)
```

在 `<template>` 中在状态栏末尾追加：
```vue
<span v-if="loadMode && loadMode !== 'normal'" class="status-bar__badge">
  {{ t('status.largeImageMode') }}
</span>
```

为 badge 加样式（在 `<style scoped>` 末尾追加）：
```css
.status-bar__badge {
  padding: 1px 6px;
  border-radius: 4px;
  background: color-mix(in srgb, #1677ff 15%, transparent);
  color: #1677ff;
  font-size: 11px;
}
```

---

## Task 7：调试入口（临时）

**Goal:** 在 window 暴露 `__picseeDebugOpen(path)` 供开发期端到端验证。

**Files:**
- Modify: `src/main.ts`（或在 App.vue onMounted 中注入）

### Step 7.1：在 main.ts 末尾追加（开发期临时）

```typescript
// TODO M4-debug：开发期调试入口，后续移除
if (import.meta.env.DEV) {
  const { useLargeImage } = await import('@/composables/useLargeImage')
  const { useDirectoryStore } = await import('@/stores/directory')
  ;(window as Record<string, unknown>).__picseeDebugOpen = async (path: string) => {
    const entry = {
      path,
      name: path.split('/').pop() ?? path,
      size: 0,
      modified: Date.now(),
    }
    const { openImage } = useLargeImage()
    await openImage(entry)
    console.log('[PicSee] __picseeDebugOpen complete', path)
  }
}
```

**注意：** 仅在 `import.meta.env.DEV` 下执行，生产构建自动 tree-shake 掉。

---

## Task 8：测试 BMP 生成脚本

**Goal:** 生成 `scripts/gen_test_bmp.mjs`，输出参数化渐变 BMP 到 `test-assets/`；`test-assets/` 加入 `.gitignore`。

**Files:**
- Create: `scripts/gen_test_bmp.mjs`
- Modify: `.gitignore`
- Create（目录）: `test-assets/`

### Step 8.1：创建 scripts/gen_test_bmp.mjs

```javascript
#!/usr/bin/env node
/**
 * gen_test_bmp.mjs — 生成参数化渐变 BMP 测试文件
 *
 * 用法：
 *   node scripts/gen_test_bmp.mjs [width] [height] [output]
 *   node scripts/gen_test_bmp.mjs 14000 10000 test-assets/test-400mb.bmp
 *
 * 默认：14000×10000，输出到 test-assets/test-large.bmp
 * 生成 24bit 未压缩 BMP（BI_RGB），渐变图案（R=x%256, G=y%256, B=0）
 *
 * 注意：生成的 BMP 文件已加入 .gitignore，不提交到版本库。
 */

import { createWriteStream, mkdirSync } from 'node:fs'
import { dirname } from 'node:path'

const width = parseInt(process.argv[2] ?? '14000', 10)
const height = parseInt(process.argv[3] ?? '10000', 10)
const output = process.argv[4] ?? 'test-assets/test-large.bmp'

const bpp = 3 // 24bit BGR
const rowStride = Math.ceil((width * bpp) / 4) * 4 // 4 字节对齐
const pixelDataSize = rowStride * height
const fileSize = 54 + pixelDataSize

console.log(`生成 BMP：${width}×${height}，大小约 ${(fileSize / (1024 ** 3)).toFixed(2)}GB`)
console.log(`输出：${output}`)

mkdirSync(dirname(output), { recursive: true })
const stream = createWriteStream(output)

// ── BMP 文件头（54 字节）──────────────────────────────────────────
const header = Buffer.alloc(54)
header.write('BM', 0, 'ascii')
header.writeUInt32LE(fileSize, 2)
header.writeUInt32LE(0, 6)         // reserved
header.writeUInt32LE(54, 10)       // pixelDataOffset
header.writeUInt32LE(40, 14)       // DIB header size
header.writeInt32LE(width, 18)     // width
header.writeInt32LE(height, 22)    // height（正数=bottom-up）
header.writeUInt16LE(1, 26)        // color planes
header.writeUInt16LE(24, 28)       // bits per pixel
header.writeUInt32LE(0, 30)        // compression (BI_RGB)
header.writeUInt32LE(pixelDataSize, 34)
header.writeInt32LE(2835, 38)      // XPelsPerMeter (~72 DPI)
header.writeInt32LE(2835, 42)      // YPelsPerMeter

stream.write(header)

// ── 像素数据（bottom-up：文件首行是图像最后一行）──────────────────
// 每次写一行，避免整图占用内存
const rowBuf = Buffer.alloc(rowStride, 0)
const CHUNK_SIZE = 100 // 每 100 行 flush 一次

let rowsWritten = 0

function writeRow(imgY) {
  // BMP bottom-up：文件第 0 行是图像第 height-1 行
  const fileRow = height - 1 - imgY
  for (let x = 0; x < width; x++) {
    const offset = x * 3
    rowBuf[offset] = 0              // B
    rowBuf[offset + 1] = imgY % 256 // G
    rowBuf[offset + 2] = x % 256    // R
  }
  stream.write(rowBuf.slice(0, rowStride))
  rowsWritten++
  if (rowsWritten % 1000 === 0) {
    process.stdout.write(`\r  写入行 ${rowsWritten}/${height} (${(rowsWritten / height * 100).toFixed(1)}%)`)
  }
}

// BMP bottom-up：文件中行从图像最后一行（imgY=height-1）到第一行（imgY=0）
// 为了让输出的 imgY 从大到小对应文件行从小到大，正向遍历 file_row
// file_row = height - 1 - imgY  →  imgY = height - 1 - file_row
function writeAllRows() {
  for (let imgY = height - 1; imgY >= 0; imgY--) {
    // 此 imgY 对应文件行 file_row = height - 1 - imgY（从 0 开始）
    // 按文件顺序写（底行先写）
    for (let x = 0; x < width; x++) {
      const offset = x * 3
      rowBuf[offset] = 0              // B
      rowBuf[offset + 1] = imgY % 256 // G
      rowBuf[offset + 2] = x % 256    // R
    }
    const ok = stream.write(rowBuf.slice(0, rowStride))
    rowsWritten++
    if (rowsWritten % 1000 === 0) {
      process.stdout.write(`\r  写入行 ${rowsWritten}/${height} (${(rowsWritten / height * 100).toFixed(1)}%)`)
    }
  }
}

writeAllRows()
stream.end(() => {
  console.log(`\n完成！文件大小：${(fileSize / (1024 ** 3)).toFixed(3)}GB → ${output}`)
})
```

### Step 8.2：更新 .gitignore

在 `.gitignore` 末尾追加：
```
# 测试用大图文件（不提交到版本库）
test-assets/
```

### Step 8.3：创建 test-assets 目录并生成测试 BMP

```bash
mkdir -p /Users/wxy/projects/my/projects/PicSee/test-assets
cd /Users/wxy/projects/my/projects/PicSee && \
  node scripts/gen_test_bmp.mjs 14000 10000 test-assets/test-large.bmp
```

注意：14000×10000×3 ≈ 400MB，生成约需 30-60 秒（取决于 IO 速度）。

---

## Task 9：验证

### Step 9.1：vue-tsc 构建

```bash
cd /Users/wxy/projects/my/projects/PicSee && npm run build 2>&1 | tail -10
```

预期：0 errors，成功。

### Step 9.2：cargo check（不改后端，确认仍通过）

```bash
cd /Users/wxy/projects/my/projects/PicSee/src-tauri && cargo check 2>&1 | tail -5
```

预期：0 errors。

### Step 9.3：启动 tauri dev 并进行端到端验证

```bash
cd /Users/wxy/projects/my/projects/PicSee && npm run tauri dev 2>&1 &
```

等应用启动后，在 DevTools 控制台执行：

```javascript
// 测试大图链路
await window.__picseeDebugOpen('/absolute/path/to/test-assets/test-large.bmp')
```

在 tauri dev 的 stdout 中应能看到：
```
[PicSee] probe_image: test-large.bmp → loadMode=tileRequired, 14000×10000, XXms
[PicSee] open_large_image: sessionId=1, gen=1, 14000×10000, preview 加载耗时=XXXXms
```

**验收标准（贴入报告的日志内容）：**
1. probe 耗时（`probe_image` 日志中的 ms 数）
2. preview 显示耗时（`open_large_image` 日志中的 ms 数；预期 ≤ 2000ms）
3. 首屏 tile 数量（从 `loadTile` 调用次数统计；在 render 函数里临时 console.log 可见范围 tile 数）
4. canvas 正确渲染 preview（目视验证：应显示蓝绿渐变图案）

---

## 已知约束与风险

1. **picsee:// URL 形态差异**：macOS WKWebView 下 URL 形态为 `picsee://localhost/...`，`largeImageUrl.ts` 的 `isMacOS()` 依赖 userAgent。若运行环境 userAgent 不含 `Mac OS X`，需调整判断逻辑（建议后续改为 `@tauri-apps/api/os` 的 `platform()` API）。

2. **tile 410 处理**：`img.onerror` 在 picsee:// 下无法直接拿到 HTTP status code，所有错误统一 `console.warn`。410（generation 过期）会被一视同仁 warn，但不影响功能（旧 tile 在 generation 校验处已被丢弃）。

3. **LargeImageCanvas 中 `watch(() => props.session, ...)` 的触发时机**：`session` prop 变化（切图时新 session 传入）时重置 previewImg/tileCache。但首次 mount 时 `immediate: false` 不触发，由 `onMounted` 中的 `loadPreview()` 负责。

4. **canvas 尺寸更新在 render 中**：canvas 物理分辨率在每次 render 开头更新，频繁 resize 时可能造成轻微闪烁。可改为在 ResizeObserver 回调中更新，但当前方案足够简单。

5. **tileSize/previewMaxSize 变化对已打开会话不回溯**：settings 中修改这两个值后，当前已打开的会话继续使用 session 中记录的值（open_large_image 返回的）。下次打开图片时生效。（已在 imageStore.setLargeImageSession 中将 session 完整记录，LargeImageCanvas 从 session prop 读取这两个值，不从 settings 读。）

6. **AppLayout 中 `openImage` 替换 `imageStore.setCurrent`**：原有对 viewerStore.resetView/preserveView 的调用逻辑保留不变，仅最后一行的 `imageStore.setCurrent(currentEntry.value)` 改为 `openImage`。切换到 null（无图）时仍调用 `imageStore.setCurrent(null)`。

7. **前端 tile 缓存 key 包含 generation**：key 为 `${sessionId}:${generation}:${tx}:${ty}`，切图后新会话的 key 完全不同，旧缓存自然失效（LRU 上限 256 个，不会无限增长）。

---

## 后端接口问题（如有，转 PM）

无新发现的后端接口问题。后端已实现的接口契约与本计划完全匹配：
- `probe_image` / `open_large_image` / `close_large_image` 已在 `lib.rs` 的 invoke_handler 注册
- picsee:// 协议已在 `register_uri_scheme_protocol` 中注册
- CSP 的 `img-src` 已包含 `picsee:`
- generation 机制（stale → 410）、session 最多 2 个、tile LRU 均已实现

潜在关注点（非缺陷，记录供参考）：
- `handle_tile_request` 中 tile 解码在 picsee:// 协议 handler 的同步上下文中执行（非 async），对大 tile 可能短暂阻塞协议处理线程。后续可改为在 handler 中返回先前缓存、未缓存时异步解码然后通知前端重新请求。当前实现在 SSD 上可接受。
