import { computed, ref, shallowRef } from 'vue'
import { defineStore } from 'pinia'
import { invoke } from '@tauri-apps/api/core'

import type {
  ImageEntry,
  OpenDirectoryCommandResult,
  OpenDirectoryResult,
  OpenImageFileResult,
  ScanDirectoryResult,
} from '@/types/image'

export const useDirectoryStore = defineStore('directory', () => {
  const currentPath = shallowRef<string | null>(null)
  const entries = ref<ImageEntry[]>([])
  const currentIndex = shallowRef(-1)
  const pendingOperations = shallowRef(0)
  const error = shallowRef<unknown | null>(null)
  let scanToken = 0

  const loading = computed(() => pendingOperations.value > 0)
  const currentEntry = computed(() => entries.value[currentIndex.value] ?? null)
  const hasPrevious = computed(() => currentIndex.value > 0)
  const hasNext = computed(() => currentIndex.value >= 0 && currentIndex.value < entries.value.length - 1)

  /** 打开单图后立即显示，再在后台补齐同目录图片列表。 */
  async function openImageFile() {
    scanToken += 1
    beginOperation()
    error.value = null
    try {
      const entry = await invoke<OpenImageFileResult>('open_image_file')
      if (!entry) return
      const directory = parentDirectory(entry.path)
      currentPath.value = directory
      entries.value = [entry]
      currentIndex.value = 0
      void scanDirectory(directory)
    } catch (reason) {
      error.value = reason
    } finally {
      endOperation()
    }
  }

  async function openDirectory() {
    scanToken += 1
    beginOperation()
    error.value = null
    try {
      const result = await invoke<OpenDirectoryCommandResult>('open_directory')
      if (!result) return
      currentPath.value = result.directory
      entries.value = result.entries
      currentIndex.value = result.entries.length ? 0 : -1
    } catch (reason) {
      error.value = reason
    } finally {
      endOperation()
    }
  }

  async function scanDirectory(path: string) {
    const token = ++scanToken
    beginOperation()
    error.value = null
    try {
      const result = await invoke<ScanDirectoryResult>('scan_directory', { path })
      if (token !== scanToken || currentPath.value !== path) return
      const targetPath = currentEntry.value?.path
      entries.value = result
      const targetIndex = targetPath ? result.findIndex(entry => entry.path === targetPath) : -1
      currentIndex.value = targetIndex >= 0 ? targetIndex : result.length ? 0 : -1
    } catch (reason) {
      if (token === scanToken) error.value = reason
    } finally {
      endOperation()
    }
  }

  async function openExternalPath(path: string) {
    scanToken += 1
    beginOperation()
    error.value = null
    try {
      const result = await invoke<OpenDirectoryResult>('open_external_path', { path })
      currentPath.value = result.directory
      entries.value = result.entries
      currentIndex.value = result.entries.length ? 0 : -1
      if (result.entries.length === 1) void scanDirectory(result.directory)
    } catch (reason) {
      error.value = reason
    } finally {
      endOperation()
    }
  }

  function removeCurrent() {
    if (currentIndex.value < 0) return
    const index = currentIndex.value
    entries.value.splice(index, 1)
    currentIndex.value = entries.value.length ? Math.min(index, entries.value.length - 1) : -1
  }

  function select(index: number) {
    if (index >= 0 && index < entries.value.length) currentIndex.value = index
  }

  function selectPrevious() {
    if (hasPrevious.value) currentIndex.value -= 1
  }

  function selectNext() {
    if (hasNext.value) currentIndex.value += 1
  }

  function beginOperation() {
    pendingOperations.value += 1
  }

  function endOperation() {
    pendingOperations.value = Math.max(0, pendingOperations.value - 1)
  }

  return {
    currentPath,
    entries,
    currentIndex,
    currentEntry,
    hasPrevious,
    hasNext,
    loading,
    error,
    openImageFile,
    openDirectory,
    scanDirectory,
    openExternalPath,
    removeCurrent,
    select,
    selectPrevious,
    selectNext,
  }
})

function parentDirectory(path: string) {
  const separatorIndex = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'))
  if (separatorIndex === 0) return path.slice(0, 1)
  return separatorIndex > 0 ? path.slice(0, separatorIndex) : path
}
