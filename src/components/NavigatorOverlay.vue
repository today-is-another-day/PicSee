<script setup lang="ts">
import { computed } from 'vue'
import { storeToRefs } from 'pinia'
import { useImageStore } from '@/stores/image'
import { useSettingsStore } from '@/stores/settings'
import { useViewerStore } from '@/stores/viewer'
import { previewUrl } from '@/utils/largeImageUrl'

const imageStore = useImageStore()
const settingsStore = useSettingsStore()
const viewerStore = useViewerStore()
const { displayImage, offset, rotation, viewport, zoom } = storeToRefs(viewerStore)

const source = computed(() => imageStore.largeImageSession
  ? previewUrl(imageStore.largeImageSession.sessionId)
  : imageStore.src)
const size = computed(() => settingsStore.settings.viewer.navigatorSize)
const scale = computed(() => size.value / Math.max(displayImage.value.width, displayImage.value.height, 1))
const width = computed(() => displayImage.value.width * scale.value)
const height = computed(() => displayImage.value.height * scale.value)
const sourceWidth = computed(() => imageStore.naturalWidth * scale.value)
const sourceHeight = computed(() => imageStore.naturalHeight * scale.value)
const visible = computed(() => {
  const mode = settingsStore.settings.viewer.navigatorMode
  return imageStore.hasImage && mode !== 'hidden' && (mode === 'always' || viewerStore.canPan)
})
const rect = computed(() => ({
  x: Math.max(0, -offset.value.x / zoom.value * scale.value),
  y: Math.max(0, -offset.value.y / zoom.value * scale.value),
  width: Math.min(width.value, viewport.value.width / zoom.value * scale.value),
  height: Math.min(height.value, viewport.value.height / zoom.value * scale.value),
}))

function navigate(event: PointerEvent) {
  const bounds = (event.currentTarget as HTMLElement).getBoundingClientRect()
  viewerStore.setNormalizedCenter(
    (event.clientX - bounds.left) / bounds.width,
    (event.clientY - bounds.top) / bounds.height,
  )
}

const imageTransform = computed(() => {
  if (rotation.value === 90) return `translate(${sourceHeight.value}px, 0) rotate(90deg)`
  if (rotation.value === 180) return `translate(${sourceWidth.value}px, ${sourceHeight.value}px) rotate(180deg)`
  if (rotation.value === 270) return `translate(0, ${sourceWidth.value}px) rotate(270deg)`
  return 'none'
})
</script>

<template>
  <div
    v-if="visible && source"
    class="navigator"
    :style="{ width: `${width}px`, height: `${height}px` }"
    @pointerdown.prevent="navigate"
    @pointermove.left.prevent="navigate"
  >
    <img :src="source" :style="{ width: `${sourceWidth}px`, height: `${sourceHeight}px`, transform: imageTransform }" draggable="false">
    <div class="navigator__shade" />
    <div class="navigator__rect" :style="{ left: `${rect.x}px`, top: `${rect.y}px`, width: `${rect.width}px`, height: `${rect.height}px` }" />
  </div>
</template>

<style scoped>
.navigator {
  position: absolute;
  right: 16px;
  bottom: 16px;
  z-index: 4;
  overflow: hidden;
  border: 1px solid rgb(255 255 255 / 55%);
  border-radius: 4px;
  background: #111;
  box-shadow: 0 2px 12px rgb(0 0 0 / 45%);
  cursor: crosshair;
}
.navigator img { position: absolute; top: 0; left: 0; max-width: none; transform-origin: 0 0; pointer-events: none; }
.navigator__shade { position: absolute; inset: 0; background: rgb(0 0 0 / 35%); }
.navigator__rect { position: absolute; border: 1px solid #fff; background: rgb(255 255 255 / 12%); box-shadow: 0 0 0 1px rgb(0 0 0 / 45%); }
</style>
