import { computed, shallowRef } from 'vue'
import { defineStore } from 'pinia'
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
   * 设置当前图片（重置状态）。
   *
   * loadMode=normal → 走现有 <img> 路径（src 赋值由 setNormalMode 完成）。
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
    naturalWidth.value = 0
    naturalHeight.value = 0

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
    useViewerStore().setMaxZoom(32)
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
    const viewerStore = useViewerStore()
    viewerStore.setMaxZoom(session.rawPreview ? 1 : 32)
    viewerStore.setImageSize(width, height)
    if (viewerStore.displayMode !== 'custom') viewerStore.applyDisplayMode(viewerStore.displayMode)
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
