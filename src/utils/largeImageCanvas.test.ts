import { describe, expect, it } from 'vitest'
import { evictByBytes, pickLevel, visibleImageRect } from './largeImageCanvas'

describe('pickLevel', () => {
  it('selects the ideal level on first render', () => {
    expect(pickLevel(1, null, 6)).toBe(0)
    expect(pickLevel(0.4, null, 6)).toBe(1)
    expect(pickLevel(0.1, null, 6)).toBe(3)
  })

  it('keeps the previous level inside the hysteresis band', () => {
    expect(pickLevel(0.5, 0, 6)).toBe(0)
    expect(pickLevel(0.5, 1, 6)).toBe(1)
    expect(pickLevel(0.2, 2, 6)).toBe(2)
  })

  it('moves one level when the current sampling rate crosses a threshold', () => {
    expect(pickLevel(0.44, 0, 6)).toBe(1)
    expect(pickLevel(0.56, 1, 6)).toBe(0)
    expect(pickLevel(0.1, 2, 6)).toBe(3)
  })

  it('walks through multiple levels until the sampling rate is stable', () => {
    expect(pickLevel(0.1, 0, 6)).toBe(3)
    expect(pickLevel(0.01, 0, 6)).toBe(6)
  })
})

describe('visibleImageRect', () => {
  const viewport = { width: 40, height: 20 }
  const offset = { x: 10, y: 5 }

  it('maps viewport corners through rotation 0', () => {
    expect(visibleImageRect(viewport, 1, offset, 0, 100, 80)).toEqual({
      imgX: 0,
      imgY: 0,
      imgX1: 30,
      imgY1: 15,
    })
  })

  it('maps viewport corners through rotation 90', () => {
    expect(visibleImageRect(viewport, 1, offset, 90, 100, 80)).toEqual({
      imgX: 0,
      imgY: 50,
      imgX1: 15,
      imgY1: 80,
    })
  })

  it('maps viewport corners through rotation 180', () => {
    expect(visibleImageRect(viewport, 1, offset, 180, 100, 80)).toEqual({
      imgX: 70,
      imgY: 65,
      imgX1: 100,
      imgY1: 80,
    })
  })

  it('maps viewport corners through rotation 270', () => {
    expect(visibleImageRect(viewport, 1, offset, 270, 100, 80)).toEqual({
      imgX: 85,
      imgY: 0,
      imgX1: 100,
      imgY1: 30,
    })
  })

  it('accounts for zoom and offset when mapping viewport corners', () => {
    expect(visibleImageRect(viewport, 2, { x: -20, y: -10 }, 0, 100, 80)).toEqual({
      imgX: 10,
      imgY: 5,
      imgX1: 30,
      imgY1: 15,
    })
  })
})

describe('evictByBytes', () => {
  it('evicts oldest entries until the cache fits the byte budget', () => {
    const cache = new Map([
      ['oldest', { bytes: 40 }],
      ['middle', { bytes: 30 }],
      ['newest', { bytes: 20 }],
    ])

    const bytes = evictByBytes(cache, 90, 50)

    expect(bytes).toBe(50)
    expect([...cache.keys()]).toEqual(['middle', 'newest'])
  })

  it('skips pinned entries while evicting oldest unpinned entries', () => {
    const cache = new Map([
      ['pinned-oldest', { bytes: 40 }],
      ['middle', { bytes: 30 }],
      ['newest', { bytes: 20 }],
    ])

    const bytes = evictByBytes(cache, 90, 60, new Set(['pinned-oldest']))

    expect(bytes).toBe(60)
    expect([...cache.keys()]).toEqual(['pinned-oldest', 'newest'])
  })

  it('does not evict when the byte budget is sufficient', () => {
    const cache = new Map([
      ['oldest', { bytes: 40 }],
      ['newest', { bytes: 20 }],
    ])

    const bytes = evictByBytes(cache, 60, 60)

    expect(bytes).toBe(60)
    expect([...cache.keys()]).toEqual(['oldest', 'newest'])
  })

  it('returns early for an empty cache', () => {
    expect(evictByBytes(new Map(), 0, 50)).toBe(0)
  })
})
