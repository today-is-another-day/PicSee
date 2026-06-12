import { ref, shallowRef } from 'vue'
import { defineStore } from 'pinia'

export const useDirectoryStore = defineStore('directory', () => {
  const currentPath = shallowRef<string | null>(null)
  const entries = ref<string[]>([])
  return { currentPath, entries }
})
