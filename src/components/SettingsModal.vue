<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { message } from 'ant-design-vue'
import { storeToRefs } from 'pinia'
import { useI18n } from 'vue-i18n'

import { useAppStore } from '@/stores/app'
import { DEFAULT_SETTINGS, useSettingsStore } from '@/stores/settings'
import type { AppSettings } from '@/types/settings'

const { t } = useI18n()
const appStore = useAppStore()
const settingsStore = useSettingsStore()
const { settings, saving } = storeToRefs(settingsStore)
const draft = ref<AppSettings>(structuredClone(settings.value))

watch(
  () => appStore.settingsVisible,
  () => {
    draft.value = structuredClone(settings.value)
  },
)

const languageOptions = computed(() => [
  { value: 'system', label: t('option.system') },
  { value: 'zh-CN', label: t('option.zhCN') },
  { value: 'en-US', label: t('option.enUS') },
])
const themeOptions = computed(() => [
  { value: 'system', label: t('option.system') },
  { value: 'light', label: t('option.light') },
  { value: 'dark', label: t('option.dark') },
])
const zoomOptions = computed(() => [
  { value: 'fit-window', label: t('option.fitWindow') },
  { value: 'fit-width', label: t('option.fitWidth') },
  { value: 'actual-size', label: t('option.actualSize') },
  { value: 'remember', label: t('option.remember') },
])
const thumbnailPositionOptions = computed(() => [
  { value: 'left', label: t('option.left') },
  { value: 'bottom', label: t('option.bottom') },
])
const thumbnailSizeOptions = computed(() => [
  { value: 96, label: t('option.thumbnail96') },
  { value: 160, label: t('option.thumbnail160') },
  { value: 256, label: t('option.thumbnail256') },
])
const navigatorModeOptions = computed(() => [
  { value: 'always', label: t('option.navigatorAlways') },
  { value: 'auto', label: t('option.navigatorAuto') },
  { value: 'hidden', label: t('option.navigatorHidden') },
])
const navigatorSizeOptions = [160, 200, 240].map(value => ({ value, label: `${value}px` }))

async function handleSave() {
  try {
    await settingsStore.saveSettings(draft.value)
    message.success(t('settings.saved'))
    appStore.closeSettings()
  } catch (error) {
    console.warn('Unable to save settings.', error)
    message.error(t('settings.saveFailed'))
  }
}

function handleReset() {
  draft.value = structuredClone(DEFAULT_SETTINGS)
  message.success(t('settings.resetDone'))
}

function handleCancel() {
  appStore.closeSettings()
}
</script>

<template>
  <a-modal
    :open="appStore.settingsVisible"
    :title="t('settings.title')"
    :width="760"
    centered
    @cancel="handleCancel"
  >
    <a-tabs class="settings-tabs" tab-position="left">
      <a-tab-pane key="general" :tab="t('settings.group.general')">
        <a-form layout="vertical">
          <p class="settings-description">{{ t('settings.generalDescription') }}</p>
          <a-form-item :label="t('settings.showStatusBar')"><a-switch v-model:checked="draft.layout.showStatusBar" /></a-form-item>
          <a-form-item :label="t('settings.compactMode')"><a-switch v-model:checked="draft.layout.compactMode" /></a-form-item>
        </a-form>
      </a-tab-pane>

      <a-tab-pane key="appearance" :tab="t('settings.group.appearance')">
        <a-form layout="vertical">
          <a-form-item :label="t('settings.theme')"><a-select v-model:value="draft.theme" :options="themeOptions" /></a-form-item>
        </a-form>
      </a-tab-pane>

      <a-tab-pane key="viewer" :tab="t('settings.group.viewer')">
        <a-form layout="vertical">
          <a-form-item :label="t('settings.defaultZoomMode')">
            <a-select v-model:value="draft.viewer.defaultZoomMode" :options="zoomOptions" />
          </a-form-item>
          <a-form-item :label="t('settings.zoomStep')">
            <a-input-number v-model:value="draft.viewer.zoomStep" :min="0.01" :max="1" :step="0.01" />
          </a-form-item>
          <a-form-item :label="t('settings.smoothZoom')"><a-switch v-model:checked="draft.viewer.smoothZoom" /></a-form-item>
          <a-form-item :label="t('settings.zoomToCursor')"><a-switch v-model:checked="draft.viewer.zoomToCursor" /></a-form-item>
          <a-form-item :label="t('settings.resetZoomOnSwitch')"><a-switch v-model:checked="draft.viewer.resetZoomOnSwitch" /></a-form-item>
          <a-form-item :label="t('settings.navigatorMode')"><a-select v-model:value="draft.viewer.navigatorMode" :options="navigatorModeOptions" /></a-form-item>
          <a-form-item :label="t('settings.navigatorSize')"><a-select v-model:value="draft.viewer.navigatorSize" :options="navigatorSizeOptions" /></a-form-item>
          <a-form-item :label="t('settings.confirmDelete')"><a-switch v-model:checked="draft.viewer.confirmDelete" /></a-form-item>
        </a-form>
      </a-tab-pane>

      <a-tab-pane key="large-image" :tab="t('settings.group.largeImage')">
        <a-form layout="vertical">
          <div class="settings-grid">
            <a-form-item :label="t('settings.fileSizeThresholdMB')"><a-input-number v-model:value="draft.largeImage.fileSizeThresholdMB" :min="1" /></a-form-item>
            <a-form-item :label="t('settings.pixelThreshold')"><a-input-number v-model:value="draft.largeImage.pixelThreshold" :min="1" /></a-form-item>
            <a-form-item :label="t('settings.sideThreshold')"><a-input-number v-model:value="draft.largeImage.sideThreshold" :min="1" /></a-form-item>
            <a-form-item :label="t('settings.previewMaxSize')"><a-select v-model:value="draft.largeImage.previewMaxSize" :options="[2048, 4096, 8192].map(value => ({ value, label: String(value) }))" /></a-form-item>
            <a-form-item :label="t('settings.tileSize')"><a-select v-model:value="draft.largeImage.tileSize" :options="[256, 512, 1024].map(value => ({ value, label: String(value) }))" /></a-form-item>
            <a-form-item :label="t('settings.prefetchRadius')"><a-input-number v-model:value="draft.largeImage.prefetchRadius" :min="0" /></a-form-item>
          </div>
          <a-form-item :label="t('settings.enableTilePrefetch')"><a-switch v-model:checked="draft.largeImage.enableTilePrefetch" /></a-form-item>
        </a-form>
      </a-tab-pane>

      <a-tab-pane key="thumbnail" :tab="t('settings.group.thumbnail')">
        <a-form layout="vertical">
          <a-form-item :label="t('settings.thumbnailPosition')">
            <a-select v-model:value="draft.layout.thumbnailPosition" :options="thumbnailPositionOptions" />
          </a-form-item>
          <a-form-item :label="t('settings.thumbnailSize')">
            <a-select v-model:value="draft.layout.thumbnailSize" :options="thumbnailSizeOptions" />
          </a-form-item>
          <a-form-item :label="t('settings.showThumbnailBar')"><a-switch v-model:checked="draft.layout.showThumbnailBar" /></a-form-item>
        </a-form>
      </a-tab-pane>

      <a-tab-pane key="cache" :tab="t('settings.group.cache')">
        <a-form layout="vertical">
          <div class="settings-grid">
            <a-form-item :label="t('settings.memoryCacheLimitMB')"><a-input-number v-model:value="draft.cache.memoryCacheLimitMB" :min="0" /></a-form-item>
            <a-form-item :label="t('settings.diskCacheLimitMB')"><a-input-number v-model:value="draft.cache.diskCacheLimitMB" :min="0" /></a-form-item>
          </div>
          <a-form-item :label="t('settings.enableDiskCache')"><a-switch v-model:checked="draft.cache.enableDiskCache" /></a-form-item>
          <a-form-item :label="t('settings.clearTempTileOnExit')"><a-switch v-model:checked="draft.cache.clearTempTileOnExit" /></a-form-item>
        </a-form>
      </a-tab-pane>

      <a-tab-pane key="performance" :tab="t('settings.group.performance')">
        <a-form layout="vertical">
          <div class="settings-grid">
            <a-form-item :label="t('settings.tileConcurrency')"><a-input-number v-model:value="draft.performance.tileConcurrency" :min="1" /></a-form-item>
            <a-form-item :label="t('settings.decodeConcurrency')"><a-input-number v-model:value="draft.performance.decodeConcurrency" :min="1" /></a-form-item>
            <a-form-item :label="t('settings.thumbnailConcurrency')"><a-input-number v-model:value="draft.performance.thumbnailConcurrency" :min="1" /></a-form-item>
            <a-form-item :label="t('settings.preloadNormalCount')"><a-input-number v-model:value="draft.performance.preloadNormalCount" :min="0" /></a-form-item>
            <a-form-item :label="t('settings.preloadLargePreviewCount')"><a-input-number v-model:value="draft.performance.preloadLargePreviewCount" :min="0" /></a-form-item>
          </div>
        </a-form>
      </a-tab-pane>

      <a-tab-pane key="shortcuts" :tab="t('settings.group.shortcuts')">
        <a-empty :description="t('settings.shortcutsDescription')" />
      </a-tab-pane>

      <a-tab-pane key="language" :tab="t('settings.group.language')">
        <a-form layout="vertical">
          <a-form-item :label="t('settings.language')"><a-select v-model:value="draft.language" :options="languageOptions" /></a-form-item>
        </a-form>
      </a-tab-pane>

      <a-tab-pane key="about" :tab="t('settings.group.about')">
        <a-result status="info" title="PicSee" :sub-title="t('settings.aboutDescription')" />
      </a-tab-pane>
    </a-tabs>

    <template #footer>
      <div class="settings-footer">
        <a-button @click="handleReset">{{ t('action.reset') }}</a-button>
        <div>
          <a-button @click="handleCancel">{{ t('action.cancel') }}</a-button>
          <a-button type="primary" :loading="saving" @click="handleSave">{{ t('action.save') }}</a-button>
        </div>
      </div>
    </template>
  </a-modal>
</template>

<style scoped>
.settings-tabs {
  min-height: 480px;
}

.settings-tabs :deep(.ant-tabs-content-holder) {
  max-height: 58vh;
  padding-inline: 20px 4px;
  overflow: auto;
}

.settings-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 0 18px;
}

.settings-grid :deep(.ant-input-number) {
  width: 100%;
}

.settings-description {
  color: var(--muted-color);
}

.settings-footer,
.settings-footer div {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}

.settings-footer {
  width: 100%;
}
</style>
