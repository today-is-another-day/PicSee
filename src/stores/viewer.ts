import { reactive, shallowRef } from 'vue'
import { defineStore } from 'pinia'

export const useViewerStore = defineStore('viewer', () => {
  const zoom = shallowRef(1)
  const rotation = shallowRef(0)
  const offset = reactive({ x: 0, y: 0 })

  function resetView() {
    zoom.value = 1
    rotation.value = 0
    offset.x = 0
    offset.y = 0
  }

  return { zoom, rotation, offset, resetView }
})
