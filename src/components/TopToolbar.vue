<script setup lang="ts">
import { useI18n } from 'vue-i18n'

import logoUrl from '@/assets/logo.svg'
import { useAppStore } from '@/stores/app'
import { useDirectoryStore } from '@/stores/directory'
import { useSettingsStore } from '@/stores/settings'
import { useViewerStore } from '@/stores/viewer'

const { t } = useI18n()
const appStore = useAppStore()
const directoryStore = useDirectoryStore()
const settingsStore = useSettingsStore()
const viewerStore = useViewerStore()
</script>

<template>
  <header class="top-toolbar">
    <div class="top-toolbar__brand">
      <img class="top-toolbar__logo" :src="logoUrl" alt="" draggable="false" />
      <div>
        <strong>{{ t('app.name') }}</strong>
      </div>
    </div>
    <div class="top-toolbar__actions">
      <a-button @click="directoryStore.openImageFile">{{ t('action.openFile') }}</a-button>
      <a-button :loading="directoryStore.loading" @click="directoryStore.openDirectory">{{ t('action.openDirectory') }}</a-button>
      <a-button-group>
        <a-button :aria-label="t('action.previous')" :disabled="!directoryStore.hasPrevious" @click="directoryStore.selectPrevious">{{ t('action.previous') }}</a-button>
        <a-button :aria-label="t('action.next')" :disabled="!directoryStore.hasNext" @click="directoryStore.selectNext">{{ t('action.next') }}</a-button>
      </a-button-group>
      <a-button-group>
        <a-button :aria-label="t('action.zoomOut')" @click="viewerStore.zoomOut(settingsStore.settings.viewer.zoomStep)">−</a-button>
        <a-button @click="viewerStore.applyDisplayMode('fit-window')">{{ t('action.fitWindow') }}</a-button>
        <a-button @click="viewerStore.applyDisplayMode('actual-size')">100%</a-button>
        <a-button :aria-label="t('action.zoomIn')" @click="viewerStore.zoomIn(settingsStore.settings.viewer.zoomStep)">+</a-button>
      </a-button-group>
      <a-button type="primary" @click="appStore.openSettings">{{ t('action.settings') }}</a-button>
    </div>
  </header>
</template>

<style scoped>
.top-toolbar {
  display: flex;
  min-height: 64px;
  align-items: center;
  justify-content: space-between;
  gap: 20px;
  padding: 10px 18px;
  border-bottom: 1px solid var(--border-color);
  background: var(--panel-bg);
}

.top-toolbar__brand,
.top-toolbar__actions {
  display: flex;
  align-items: center;
  gap: 10px;
}

.top-toolbar__brand div {
  display: grid;
}

.top-toolbar__brand span {
  color: var(--muted-color);
  font-size: 12px;
}

.top-toolbar__logo {
  width: 28px;
  height: 28px;
  object-fit: contain;
  user-select: none;
  -webkit-user-drag: none;
}
</style>
