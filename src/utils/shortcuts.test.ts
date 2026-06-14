import { describe, expect, it } from 'vitest'

import {
  DEFAULT_SHORTCUTS,
  eventToChord,
  findConflicts,
  formatChord,
} from './shortcuts'

function keyboardEvent(init: KeyboardEventInit): KeyboardEvent {
  return {
    altKey: false,
    code: '',
    ctrlKey: false,
    key: '',
    metaKey: false,
    shiftKey: false,
    ...init,
  } as KeyboardEvent
}

describe('eventToChord', () => {
  it('uses logical modifiers and event.code in a fixed order', () => {
    expect(eventToChord(keyboardEvent({ code: 'KeyR', shiftKey: true }))).toBe('Shift+KeyR')
    expect(eventToChord(keyboardEvent({ code: 'KeyR' }))).toBe('KeyR')
    expect(eventToChord(keyboardEvent({ altKey: true, code: 'KeyO', metaKey: true, shiftKey: true })))
      .toBe('Mod+Alt+Shift+KeyO')
  })

  it('ignores modifier-only key presses', () => {
    expect(eventToChord(keyboardEvent({ code: 'ShiftLeft', key: 'Shift', shiftKey: true }))).toBeNull()
    expect(eventToChord(keyboardEvent({ code: 'MetaLeft', key: 'Meta', metaKey: true }))).toBeNull()
  })
})

describe('formatChord', () => {
  it('formats mac chords', () => {
    expect(formatChord('Mod+Shift+KeyR', true)).toBe('⌘ ⇧ R')
    expect(formatChord('ArrowLeft', true)).toBe('←')
  })

  it('formats non-mac chords', () => {
    expect(formatChord('Mod+Alt+Equal', false)).toBe('Ctrl Alt =')
    expect(formatChord('Minus', false)).toBe('−')
  })
})

describe('findConflicts', () => {
  it('returns only chords assigned to multiple actions', () => {
    const keymap = { ...DEFAULT_SHORTCUTS, next: 'ArrowLeft' }
    expect(findConflicts(keymap)).toEqual({
      ArrowLeft: ['previous', 'next'],
    })
  })
})
