export const ACTION_IDS = [
  'openFile', 'openDirectory', 'settings', 'reveal', 'copyFile', 'delete',
  'previous', 'next', 'zoomIn', 'zoomOut', 'fitWindow', 'actualSize',
  'fullscreen', 'rotateClockwise', 'rotateCounterClockwise',
] as const

export type ActionId = typeof ACTION_IDS[number]

export const DEFAULT_SHORTCUTS: Record<ActionId, string> = {
  openFile: 'Mod+KeyO',
  openDirectory: 'Mod+Shift+KeyO',
  settings: 'Mod+Comma',
  reveal: 'Mod+Shift+KeyR',
  copyFile: 'Mod+KeyC',
  delete: 'Delete',
  previous: 'ArrowLeft',
  next: 'ArrowRight',
  zoomIn: 'Equal',
  zoomOut: 'Minus',
  fitWindow: 'Digit0',
  actualSize: 'Digit1',
  fullscreen: 'KeyF',
  rotateClockwise: 'KeyR',
  rotateCounterClockwise: 'Shift+KeyR',
}

const MODIFIER_CODES = new Set([
  'AltLeft', 'AltRight', 'ControlLeft', 'ControlRight',
  'MetaLeft', 'MetaRight', 'ShiftLeft', 'ShiftRight',
])

export function eventToChord(event: KeyboardEvent): string | null {
  if (MODIFIER_CODES.has(event.code) || ['Alt', 'Control', 'Meta', 'Shift'].includes(event.key)) {
    return null
  }
  const parts: string[] = []
  if (event.metaKey || event.ctrlKey) parts.push('Mod')
  if (event.altKey) parts.push('Alt')
  if (event.shiftKey) parts.push('Shift')
  parts.push(event.code)
  return parts.join('+')
}

export function formatChord(chord: string, isMac: boolean): string {
  return chord.split('+').map((part) => {
    if (part === 'Mod') return isMac ? '⌘' : 'Ctrl'
    if (part === 'Shift') return '⇧'
    if (part === 'Alt') return isMac ? '⌥' : 'Alt'
    if (/^Key[A-Z]$/.test(part)) return part.slice(3)
    if (/^Digit[0-9]$/.test(part)) return part.slice(5)
    return {
      Equal: '=', Minus: '−', Comma: ',',
      ArrowLeft: '←', ArrowRight: '→', ArrowUp: '↑', ArrowDown: '↓',
      Space: 'Space', Delete: 'Delete', Escape: 'Esc',
    }[part] ?? part.replace(/^(Key|Digit)/, '')
  }).join(' ')
}

export function resolveAction(chord: string, keymap: Record<string, string>): ActionId | null {
  return ACTION_IDS.find(action => keymap[action] === chord) ?? null
}

export function findConflicts(keymap: Record<string, string>): Record<string, ActionId[]> {
  const grouped: Record<string, ActionId[]> = {}
  for (const action of ACTION_IDS) {
    const chord = keymap[action]
    if (chord) (grouped[chord] ??= []).push(action)
  }
  return Object.fromEntries(Object.entries(grouped).filter(([, actions]) => actions.length > 1))
}
