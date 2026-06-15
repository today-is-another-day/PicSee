interface PathEntry {
  path: string
}

/** 按距离由近到远收集当前项前后的邻居路径，并保持路径唯一。 */
export function collectNeighborPaths(entries: readonly PathEntry[], currentIndex: number, count: number): string[] {
  if (count <= 0 || currentIndex < 0 || currentIndex >= entries.length) return []

  const paths = new Set<string>()
  for (let distance = 1; distance <= count; distance++) {
    for (const offset of [-distance, distance]) {
      const target = entries[currentIndex + offset]
      if (target) paths.add(target.path)
    }
  }
  return [...paths]
}
