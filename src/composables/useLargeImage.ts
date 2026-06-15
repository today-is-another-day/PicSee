import { invoke } from '@tauri-apps/api/core'
import { i18n } from '@/i18n'
import { useImageStore } from '@/stores/image'
import type {
  ImageEntry,
  ImageProbe,
  LargeImageBackendError,
  LargeImageSession,
  OpenLargeImageResult,
} from '@/types/image'

/** 调用 token，每次 openImage 递增，用于取消旧的 in-flight 请求。 */
let currentToken = 0
/** 普通 <img> 加载失败后，每个路径只尝试一次后端解码回退。 */
const normalDecodeFallbackPaths = new Set<string>()

function localizeError(reason: unknown): Error {
  const backend = reason as Partial<LargeImageBackendError> | null
  if (backend && typeof backend.code === 'string') {
    const key = `largeImage.errors.${backend.code}`
    const translated = i18n.global.t(key)
    return new Error(translated === key ? backend.message ?? String(reason) : translated)
  }
  return reason instanceof Error ? reason : new Error(String(reason))
}

/**
 * 普通 <img> 解码失败时，改走后端通用解码路径。
 *
 * 返回 false 表示该路径已经尝试过回退，调用方应维持原错误处理。
 */
export async function fallbackNormalToDecoded(entry: ImageEntry): Promise<boolean> {
  if (normalDecodeFallbackPaths.has(entry.path)) return false
  normalDecodeFallbackPaths.add(entry.path)

  const token = ++currentToken
  const imageStore = useImageStore()
  try {
    const result = await invoke<OpenLargeImageResult>('open_large_image', { path: entry.path })
    if (token !== currentToken) {
      void invoke('close_large_image', { sessionId: result.sessionId }).catch(() => {})
      return true
    }

    const session: LargeImageSession = { ...result, path: entry.path }
    imageStore.setLargeImageSession('largeCandidate', session, result.width, result.height)
  }
  catch (err) {
    if (token === currentToken) imageStore.markError(localizeError(err))
  }
  return true
}

/** TODO M4-debug：上报耗时事件到 vite dev server（DEV only）。 */
function reportE2E(data: Record<string, unknown>): void {
  if (!import.meta.env.DEV) return
  // 写入 DOM 元素供外部读取（AppleScript / osascript）
  let el = document.getElementById('__picsee_e2e_log')
  if (!el) {
    el = document.createElement('div')
    el.id = '__picsee_e2e_log'
    el.style.cssText = 'display:none'
    document.body.appendChild(el)
  }
  const line = JSON.stringify(data)
  el.textContent = (el.textContent ? el.textContent + '\n' : '') + line
  // 同时尝试 HTTP 上报（如果 capabilities 允许）
  void fetch('/__picsee_e2e_result', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: line,
  }).catch(() => {})
}

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

  function closeSession(session: LargeImageSession | null): void {
    if (!session) return
    void invoke('close_large_image', { sessionId: session.sessionId }).catch(() => {})
  }

  /**
   * 打开图片（含 probe 分流）。
   * loadMode=normal → 走 <img>；largeCandidate/tileRequired → 走大图路径。
   */
  async function openImage(entry: ImageEntry): Promise<void> {
    const oldSession = imageStore.largeImageSession
    // 1. 重置 UI 状态，显示 loading spinner
    imageStore.setCurrent(entry)

    // 2. 递增 token，取消旧 in-flight 请求
    const token = ++currentToken
    let probe: ImageProbe
    try {
      // 3. Probe：只读文件头，极快（<50ms 普通图，<5ms BMP）
      const probeStart = performance.now()
      probe = await invoke<ImageProbe>('probe_image', { path: entry.path })
      const probeMs = (performance.now() - probeStart).toFixed(0)
      if (import.meta.env.DEV) {
        console.log(
          `[PicSee] probe_image: ${entry.name} → loadMode=${probe.loadMode}, `
          + `${probe.width}×${probe.height}, ${probeMs}ms`,
        )
      }
      reportE2E({ event: 'probe', probeMs, loadMode: probe.loadMode, width: probe.width, height: probe.height })

      // token 失效（用户已切换到其他图）→ 丢弃结果
      if (token !== currentToken) {
        closeSession(oldSession)
        return
      }

      if (probe.loadMode === 'normal') {
        imageStore.setNormalMode(entry)
        closeSession(oldSession)
        return
      }
    }
    catch (err) {
      if (token !== currentToken) {
        closeSession(oldSession)
        return
      }
      closeSession(oldSession)
      imageStore.markError(localizeError(err))
      return
    }

    try {
      const openStart = performance.now()
      const result = await invoke<OpenLargeImageResult>('open_large_image', { path: entry.path })

      if (token !== currentToken) {
        closeSession(oldSession)
        // 已切图，关闭刚打开的会话
        void invoke('close_large_image', { sessionId: result.sessionId }).catch(() => {})
        return
      }

      const openMs = (performance.now() - openStart).toFixed(0)
      const session: LargeImageSession = { ...result, path: entry.path }
      imageStore.setLargeImageSession(probe.loadMode, session, result.width, result.height)
      closeSession(oldSession)

      if (import.meta.env.DEV) {
        console.log(
          `[PicSee] open_large_image: sessionId=${result.sessionId}, `
          + `${result.width}×${result.height}, open_large_image 耗时=${openMs}ms`,
        )
      }
      reportE2E({ event: 'open_large_image', openMs, sessionId: result.sessionId, width: result.width, height: result.height })
    }
    catch (err) {
      if (token !== currentToken) {
        closeSession(oldSession)
        return
      }
      if (probe.loadMode === 'largeCandidate' && probe.canFallbackToNormal) {
        imageStore.setNormalMode(entry)
        closeSession(oldSession)
        if (import.meta.env.DEV) console.warn('[PicSee] open_large_image failed; falling back to img:', err)
        reportE2E({ event: 'open_fallback', message: String(err) })
        return
      }
      closeSession(oldSession)
      console.error('[PicSee] openImage failed:', err)
      imageStore.markError(localizeError(err))
      reportE2E({ event: 'error', message: String(err) })
    }
  }

  function closeCurrentLargeImage(): void {
    currentToken++
    closeSession(imageStore.largeImageSession)
    imageStore.setCurrent(null)
  }

  return { closeCurrentLargeImage, openImage }
}
