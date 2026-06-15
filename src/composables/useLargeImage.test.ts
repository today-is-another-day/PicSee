import { createPinia, setActivePinia } from 'pinia'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { useImageStore } from '@/stores/image'
import type { ImageEntry, OpenLargeImageResult } from '@/types/image'
import { fallbackNormalToDecoded } from './useLargeImage'

const invoke = vi.hoisted(() => vi.fn())

vi.mock('@tauri-apps/api/core', () => ({
  convertFileSrc: (path: string) => `asset://${path}`,
  invoke,
}))

function entry(path: string): ImageEntry {
  return { path, name: path.split('/').at(-1) ?? path, size: 1, modified: 1 }
}

function result(sessionId: number): OpenLargeImageResult {
  return {
    sessionId,
    width: 100,
    height: 80,
    tileSize: 512,
    previewMaxSize: 2048,
    tileable: false,
    rawPreview: false,
    previewW: 100,
    previewH: 80,
  }
}

describe('fallbackNormalToDecoded', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    invoke.mockReset()
  })

  it('opens a decoded largeCandidate session for a normal image', async () => {
    const image = entry('/tmp/fallback-success.bin')
    invoke.mockResolvedValueOnce(result(1))

    await expect(fallbackNormalToDecoded(image)).resolves.toBe(true)

    const imageStore = useImageStore()
    expect(invoke).toHaveBeenCalledWith('open_large_image', { path: image.path })
    expect(imageStore.loadMode).toBe('largeCandidate')
    expect(imageStore.largeImageSession).toMatchObject({ sessionId: 1, path: image.path })
  })

  it('attempts each path only once', async () => {
    const image = entry('/tmp/fallback-once.bin')
    invoke.mockResolvedValueOnce(result(2))

    await expect(fallbackNormalToDecoded(image)).resolves.toBe(true)
    await expect(fallbackNormalToDecoded(image)).resolves.toBe(false)

    expect(invoke).toHaveBeenCalledTimes(1)
  })

  it('marks the active image as errored when decoded fallback fails', async () => {
    const image = entry('/tmp/fallback-error.bin')
    invoke.mockRejectedValueOnce(new Error('decode failed'))

    await expect(fallbackNormalToDecoded(image)).resolves.toBe(true)

    expect(useImageStore().error).toEqual(new Error('decode failed'))
  })

  it('discards a decoded session after a newer fallback invalidates its token', async () => {
    const first = entry('/tmp/fallback-stale-first.bin')
    const second = entry('/tmp/fallback-stale-second.bin')
    let resolveFirst!: (value: OpenLargeImageResult) => void
    invoke
      .mockImplementationOnce(() => new Promise<OpenLargeImageResult>((resolve) => { resolveFirst = resolve }))
      .mockResolvedValueOnce(result(4))
      .mockResolvedValueOnce(undefined)

    const firstFallback = fallbackNormalToDecoded(first)
    await fallbackNormalToDecoded(second)
    resolveFirst(result(3))
    await firstFallback

    const imageStore = useImageStore()
    expect(imageStore.largeImageSession).toMatchObject({ sessionId: 4, path: second.path })
    expect(invoke).toHaveBeenLastCalledWith('close_large_image', { sessionId: 3 })
  })
})
