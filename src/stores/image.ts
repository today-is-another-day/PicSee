import { ref, shallowRef } from 'vue'
import { defineStore } from 'pinia'

export const useImageStore = defineStore('image', () => {
  const images = ref<string[]>([])
  const currentIndex = shallowRef(-1)
  return { images, currentIndex }
})
