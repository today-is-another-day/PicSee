import { describe, expect, it } from 'vitest'
import { collectNeighborPaths } from './neighborPaths'

const entries = ['a', 'b', 'c', 'd', 'e'].map(path => ({ path }))

describe('collectNeighborPaths', () => {
  it('collects previous and next paths by increasing distance', () => {
    expect(collectNeighborPaths(entries, 2, 2)).toEqual(['b', 'd', 'a', 'e'])
  })

  it('keeps available neighbors at list boundaries', () => {
    expect(collectNeighborPaths(entries, 0, 2)).toEqual(['b', 'c'])
    expect(collectNeighborPaths(entries, 4, 2)).toEqual(['d', 'c'])
  })

  it('deduplicates repeated paths', () => {
    expect(collectNeighborPaths([{ path: 'a' }, { path: 'b' }, { path: 'a' }], 1, 1))
      .toEqual(['a'])
  })

  it('returns no paths when the count is disabled', () => {
    expect(collectNeighborPaths(entries, 2, 0)).toEqual([])
  })

  it('returns no paths when the current index is invalid', () => {
    expect(collectNeighborPaths(entries, -1, 1)).toEqual([])
    expect(collectNeighborPaths(entries, entries.length, 1)).toEqual([])
  })
})
