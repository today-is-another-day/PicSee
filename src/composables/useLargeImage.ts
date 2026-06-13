import { invoke } from '@tauri-apps/api/core'
import { useImageStore } from '@/stores/image'
import type { ImageEntry, ImageProbe, LargeImageSession, OpenLargeImageResult } from '@/types/image'

/** 调用 token，每次 openImage 递增，用于取消旧的 in-flight 请求。 */
let currentToken = 0

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
      const probeMs = (performance.now() - probeStart).toFixed(0)
      console.log(
        `[PicSee] probe_image: ${entry.name} → loadMode=${probe.loadMode}, `
        + `${probe.width}×${probe.height}, ${probeMs}ms`,
      )
      reportE2E({ event: 'probe', probeMs, loadMode: probe.loadMode, width: probe.width, height: probe.height })

      // token 失效（用户已切换到其他图）→ 丢弃结果
      if (token !== currentToken) return

      if (probe.loadMode === 'normal') {
        // 4a. 普通路径：让 <img> 加载（imageStore.setNormalMode 赋 src）
        imageStore.setNormalMode(entry)
      }
      else {
        // 4b. 大图路径（largeCandidate 第一阶段也走大图，保守省内存）
        const openStart = performance.now()
        const result = await invoke<OpenLargeImageResult>('open_large_image', { path: entry.path })

        if (token !== currentToken) {
          // 已切图，关闭刚打开的会话
          void invoke('close_large_image', { sessionId: result.sessionId }).catch(() => {})
          return
        }

        const openMs = (performance.now() - openStart).toFixed(0)
        const session: LargeImageSession = { ...result, path: entry.path }
        imageStore.setLargeImageSession(probe.loadMode, session, result.width, result.height)

        console.log(
          `[PicSee] open_large_image: sessionId=${result.sessionId}, gen=${result.generation}, `
          + `${result.width}×${result.height}, open_large_image 耗时=${openMs}ms`,
        )
        reportE2E({ event: 'open_large_image', openMs, sessionId: result.sessionId, generation: result.generation, width: result.width, height: result.height })
      }
    }
    catch (err) {
      if (token !== currentToken) return
      console.error('[PicSee] openImage failed:', err)
      imageStore.markError(err)
      reportE2E({ event: 'error', message: String(err) })
    }
  }

  return { openImage }
}
