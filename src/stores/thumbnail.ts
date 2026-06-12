import { shallowRef } from 'vue'
import { defineStore } from 'pinia'
import { invoke, convertFileSrc } from '@tauri-apps/api/core'

import type { ThumbnailBackendError } from '@/types/image'

/** 缩略图状态机。 */
export type ThumbnailStatus =
  | { status: 'idle' }
  | { status: 'loading' }
  | { status: 'loaded'; src: string }
  | { status: 'error'; code: string }

/** 单个 path 的内存缓存条目。 */
interface CacheEntry {
  state: ThumbnailStatus
  /** 若是 objectURL 则需要在淘汰时 revoke；convertFileSrc 路径不需要。 */
  isObjectUrl: boolean
}

export const useThumbnailStore = defineStore('thumbnail', () => {
  /** path → CacheEntry，用 shallowRef 避免深响应开销。 */
  const cache = shallowRef(new Map<string, CacheEntry>())
  /** 正在发起中的 invoke 数量（用于并发控制）。 */
  const activeRequests = shallowRef(0)
  /** pending 队列：等待并发位的 path 列表。 */
  const pendingQueue: string[] = []
  /** 已发起 invoke 但尚未完成的 path 集合（避免重复 invoke）。 */
  const inflightPaths = new Set<string>()

  /**
   * 获取缩略图状态（纯同步，用于模板绑定）。
   * 若该 path 尚未有状态，返回 idle。
   */
  function getState(path: string): ThumbnailStatus {
    return cache.value.get(path)?.state ?? { status: 'idle' }
  }

  /**
   * 请求加载缩略图。
   * - SVG 直接用 convertFileSrc 显示，不走后端
   * - 其他格式通过 get_thumbnail 命令生成磁盘缓存后用 convertFileSrc 显示
   * - maxConcurrency：最大并发 invoke 数（来自 settings.performance.thumbnailConcurrency）
   * - size：缩略图尺寸（来自 settings.layout.thumbnailSize）
   */
  function request(path: string, size: number, maxConcurrency: number): void {
    // 已有结果则跳过
    const existing = cache.value.get(path)
    if (existing && existing.state.status !== 'idle') return
    // 已在 in-flight 则跳过
    if (inflightPaths.has(path)) return

    // 标记 loading 并触发（或加入队列）
    setEntry(path, { status: 'loading' }, false)

    if (activeRequests.value < maxConcurrency) {
      void doLoad(path, size, maxConcurrency)
    } else {
      pendingQueue.push(path)
    }
  }

  /** 实际执行 invoke，完成后消费 pending 队列。 */
  async function doLoad(path: string, size: number, maxConcurrency: number): Promise<void> {
    activeRequests.value += 1
    inflightPaths.add(path)
    try {
      const ext = path.split('.').pop()?.toLowerCase() ?? ''
      if (ext === 'svg') {
        // SVG 直接展示原文件
        const src = convertFileSrc(path)
        setEntry(path, { status: 'loaded', src }, false)
        return
      }

      // 调用后端生成/命中缓存，返回磁盘文件绝对路径
      const cachePath = await invoke<string>('get_thumbnail', { path, size })
      const src = convertFileSrc(cachePath)
      setEntry(path, { status: 'loaded', src }, false)
    } catch (err) {
      // 解析结构化错误
      const code = extractErrorCode(err)
      setEntry(path, { status: 'error', code }, false)
    } finally {
      activeRequests.value = Math.max(0, activeRequests.value - 1)
      inflightPaths.delete(path)
      // 消费等待队列
      drainQueue(size, maxConcurrency)
    }
  }

  /** 从 pending 队列中取出下一个可处理的项目。 */
  function drainQueue(size: number, maxConcurrency: number): void {
    while (pendingQueue.length > 0 && activeRequests.value < maxConcurrency) {
      const next = pendingQueue.shift()
      if (!next) break
      // 已完成（被 evict 清掉等情况）则跳过
      const entry = cache.value.get(next)
      if (!entry || entry.state.status !== 'loading') continue
      if (inflightPaths.has(next)) continue
      void doLoad(next, size, maxConcurrency)
    }
  }

  /**
   * 淘汰不再需要的缩略图缓存（切换目录时调用）。
   * keepPaths 之外的 loaded 条目会被清除，objectURL 会被 revoke。
   */
  function evict(keepPaths: Set<string>): void {
    const next = new Map<string, CacheEntry>()
    for (const [path, entry] of cache.value) {
      if (keepPaths.has(path)) {
        next.set(path, entry)
      } else {
        // 若是 objectURL 则释放（本项目当前都是 convertFileSrc，非 objectURL）
        if (entry.isObjectUrl && entry.state.status === 'loaded') {
          URL.revokeObjectURL(entry.state.src)
        }
      }
    }
    // 清空 pending queue 中不在 keepPaths 的项目
    let i = pendingQueue.length
    while (i--) {
      if (!keepPaths.has(pendingQueue[i])) {
        pendingQueue.splice(i, 1)
      }
    }
    cache.value = next
  }

  /** 清空所有缓存（强制重置，例如设置 size 变更后需重新生成）。 */
  function reset(): void {
    for (const [, entry] of cache.value) {
      if (entry.isObjectUrl && entry.state.status === 'loaded') {
        URL.revokeObjectURL(entry.state.src)
      }
    }
    pendingQueue.length = 0
    cache.value = new Map()
  }

  /** 更新 Map 中某个 path 的状态，触发响应式更新。 */
  function setEntry(path: string, state: ThumbnailStatus, isObjectUrl: boolean): void {
    const next = new Map(cache.value)
    next.set(path, { state, isObjectUrl })
    cache.value = next
  }

  return { cache, getState, request, evict, reset }
})

/** 从 Tauri 命令抛出的错误中提取 code 字段。 */
function extractErrorCode(err: unknown): string {
  if (err && typeof err === 'object') {
    const e = err as ThumbnailBackendError
    if (typeof e.code === 'string') return e.code
  }
  if (typeof err === 'string') return err
  return 'UNKNOWN'
}
