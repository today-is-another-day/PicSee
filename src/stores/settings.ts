import { computed, ref, shallowRef, toRaw } from 'vue'
import { defineStore } from 'pinia'
import { invoke } from '@tauri-apps/api/core'

import type { AppSettings } from '@/types/settings'
import { DEFAULT_SHORTCUTS } from '@/utils/shortcuts'

export const DEFAULT_SETTINGS: AppSettings = {
  language: 'system',
  theme: 'system',
  shortcuts: { ...DEFAULT_SHORTCUTS },
  viewer: {
    defaultZoomMode: 'fit-window',
    zoomStep: 0.1,
    smoothZoom: true,
    zoomToCursor: true,
    resetZoomOnSwitch: true,
    navigatorMode: 'auto',
    navigatorSize: 200,
    confirmDelete: false,
    viewerBackground: 'dark',
    viewerBackgroundColor: '#202020',
  },
  largeImage: {
    fileSizeThresholdMB: 300,
    pixelThreshold: 50_000_000,
    sideThreshold: 12_000,
    previewMaxSize: 4096,
    tileSize: 512,
    enableTilePrefetch: true,
    prefetchRadius: 1,
  },
  cache: {
    memoryCacheLimitMB: 512,
    diskCacheLimitMB: 2048,
    enableDiskCache: true,
    clearTempTileOnExit: true,
  },
  performance: {
    tileConcurrency: 4,
    decodeConcurrency: 2,
    thumbnailConcurrency: 4,
    cpuThreads: 8,
    preloadNormalCount: 2,
    preloadLargePreviewCount: 1,
  },
  layout: {
    showThumbnailBar: true,
    thumbnailPosition: 'bottom',
    thumbnailSize: 96,
    showStatusBar: true,
    compactMode: false,
  },
}

function cloneSettings(settings: AppSettings): AppSettings {
  return structuredClone(toRaw(settings))
}

function isTauriRuntime(): boolean {
  return '__TAURI_INTERNALS__' in window
}

export const useSettingsStore = defineStore('settings', () => {
  const settings = ref<AppSettings>(cloneSettings(DEFAULT_SETTINGS))
  const loading = ref(false)
  const saving = ref(false)
  const loadError = shallowRef<unknown | null>(null)
  const persistenceAvailable = computed(isTauriRuntime)

  /** 从 Tauri 读取设置；浏览器开发环境直接使用默认值。 */
  async function loadSettings() {
    if (!isTauriRuntime()) return
    loading.value = true
    loadError.value = null
    try {
      const loaded = await invoke<AppSettings>('get_settings')
      settings.value = {
        ...loaded,
        shortcuts: { ...DEFAULT_SHORTCUTS, ...(loaded.shortcuts ?? {}) },
      }
    } catch (error) {
      loadError.value = error
      console.warn('Unable to load settings, using defaults.', error)
    } finally {
      loading.value = false
    }
  }

  /** 保存当前设置；浏览器开发环境仅保留内存中的值。 */
  async function saveSettings(nextSettings: AppSettings) {
    const candidate = cloneSettings(nextSettings)
    if (!isTauriRuntime()) {
      settings.value = candidate
      return
    }
    saving.value = true
    try {
      await invoke('save_settings', { settings: candidate })
      settings.value = candidate
    } finally {
      saving.value = false
    }
  }

  return { settings, loading, saving, loadError, persistenceAvailable, loadSettings, saveSettings }
})
