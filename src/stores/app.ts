import { shallowRef } from 'vue'
import { defineStore } from 'pinia'

export const useAppStore = defineStore('app', () => {
  const settingsVisible = shallowRef(false)

  function openSettings() { settingsVisible.value = true }
  function closeSettings() { settingsVisible.value = false }

  return { settingsVisible, openSettings, closeSettings }
})
