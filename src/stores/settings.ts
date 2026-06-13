import { computed, ref, shallowRef } from 'vue'
import { defineStore } from 'pinia'
import { invoke } from '@tauri-apps/api/core'

import type { AppSettings } from '@/types/settings'

export const DEFAULT_SETTINGS: AppSettings = {
  language: 'system',
  theme: 'system',
  viewer: {
    defaultZoomMode: 'fit-window',
    zoomStep: 0.1,
    smoothZoom: true,
    zoomToCursor: true,
    resetZoomOnSwitch: true,
    navigatorMode: 'auto',
    navigatorSize: 200,
    confirmDelete: false,
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
    preloadNormalCount: 2,
    preloadLargePreviewCount: 1,
  },
  layout: {
    showThumbnailBar: true,
    thumbnailPosition: 'bottom',
    thumbnailSize: 160,
    showStatusBar: true,
    compactMode: false,
  },
}

function cloneSettings(settings: AppSettings): AppSettings {
  return structuredClone(settings)
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
      settings.value = await invoke<AppSettings>('get_settings')
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
