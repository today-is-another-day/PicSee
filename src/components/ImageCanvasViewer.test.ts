import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const source = readFileSync(new URL('./ImageCanvasViewer.vue', import.meta.url), 'utf8')

describe('ImageCanvasViewer normal image error fallback', () => {
  it('routes normal img errors through the decoded fallback before marking an error', () => {
    expect(source).toContain("import { fallbackNormalToDecoded } from '@/composables/useLargeImage'")
    expect(source).toContain("imageStore.loadMode === 'normal'")
    expect(source).toContain('directoryStore.currentEntry')
    expect(source).toContain('fallbackNormalToDecoded(entry)')
    expect(source).toContain("imageStore.markError(new Error('image-load-failed'))")
  })
})
