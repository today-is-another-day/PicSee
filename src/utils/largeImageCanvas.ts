export interface ViewportSize {
  width: number
  height: number
}

export interface Point {
  x: number
  y: number
}

export interface ImageRect {
  imgX: number
  imgY: number
  imgX1: number
  imgY1: number
}

interface ByteSized {
  bytes: number
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max)
}

/** 按当前采样率与迟滞阈值选择下一帧 LOD 层级。 */
export function pickLevel(effScale: number, prevLevel: number | null, maxLevel: number): number {
  const maxRenderable = Math.max(0, Math.floor(maxLevel))
  const safeScale = Math.max(effScale, 1e-6)
  if (prevLevel === null || prevLevel < 0 || prevLevel > maxRenderable) {
    return clamp(Math.floor(Math.log2(1 / safeScale)), 0, maxRenderable)
  }

  let level = prevLevel
  while (level < maxRenderable && safeScale * 2 ** level < 0.45) level++
  while (level > 0 && safeScale * 2 ** level > 1.1) level--
  return level
}

/** 把视口四角经画布正变换的逆变换映射为原图坐标包围盒。 */
export function visibleImageRect(
  viewport: ViewportSize,
  zoom: number,
  offset: Point,
  rotation: number,
  imgW: number,
  imgH: number,
): ImageRect {
  const safeZoom = Math.max(zoom, 1e-6)
  const normalizedRotation = ((rotation % 360) + 360) % 360
  const corners = [
    { x: 0, y: 0 },
    { x: viewport.width, y: 0 },
    { x: 0, y: viewport.height },
    { x: viewport.width, y: viewport.height },
  ]

  const imageCorners = corners.map(({ x, y }) => {
    if (normalizedRotation === 90) {
      return {
        x: (y - offset.y) / safeZoom,
        y: imgH - (x - offset.x) / safeZoom,
      }
    }
    if (normalizedRotation === 180) {
      return {
        x: imgW - (x - offset.x) / safeZoom,
        y: imgH - (y - offset.y) / safeZoom,
      }
    }
    if (normalizedRotation === 270) {
      return {
        x: imgW - (y - offset.y) / safeZoom,
        y: (x - offset.x) / safeZoom,
      }
    }
    return {
      x: (x - offset.x) / safeZoom,
      y: (y - offset.y) / safeZoom,
    }
  })

  return {
    imgX: clamp(Math.min(...imageCorners.map(({ x }) => x)), 0, imgW),
    imgY: clamp(Math.min(...imageCorners.map(({ y }) => y)), 0, imgH),
    imgX1: clamp(Math.max(...imageCorners.map(({ x }) => x)), 0, imgW),
    imgY1: clamp(Math.max(...imageCorners.map(({ y }) => y)), 0, imgH),
  }
}

/** 按 Map 插入顺序淘汰最旧条目，直到缓存回到字节预算内。 */
export function evictByBytes<T extends ByteSized>(
  cache: Map<string, T>,
  bytes: number,
  limit: number,
  pinnedKeys: ReadonlySet<string> = new Set(),
): number {
  let remainingBytes = bytes
  while (remainingBytes > limit) {
    let oldest: string | undefined
    for (const key of cache.keys()) {
      if (!pinnedKeys.has(key)) {
        oldest = key
        break
      }
    }
    if (oldest === undefined) break
    const entry = cache.get(oldest)
    cache.delete(oldest)
    remainingBytes -= entry?.bytes ?? 0
  }
  return Math.max(0, remainingBytes)
}
