<script setup lang="ts">
import { computed } from 'vue'
import { message } from 'ant-design-vue'
import { storeToRefs } from 'pinia'
import { useI18n } from 'vue-i18n'

import { useAppStore } from '@/stores/app'
import { useSettingsStore } from '@/stores/settings'

const { t } = useI18n()
const appStore = useAppStore()
const settingsStore = useSettingsStore()
const { settings, saving } = storeToRefs(settingsStore)

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

async function handleSave() {
  try {
    await settingsStore.saveSettings()
    message.success(t('settings.saved'))
    appStore.closeSettings()
  } catch (error) {
    console.warn('Unable to save settings.', error)
    message.error(t('settings.saveFailed'))
  }
}

function handleReset() {
  settingsStore.resetSettings()
  message.success(t('settings.resetDone'))
}
</script>

<template>
  <a-modal
    v-model:open="appStore.settingsVisible"
    :title="t('settings.title')"
    :width="760"
    :confirm-loading="saving"
    :ok-text="t('action.save')"
    :cancel-text="t('action.cancel')"
    centered
    @ok="handleSave"
  >
    <a-tabs class="settings-tabs" tab-position="left">
      <a-tab-pane key="general" :tab="t('settings.group.general')">
        <a-form layout="vertical">
          <p class="settings-description">{{ t('settings.generalDescription') }}</p>
          <a-form-item :label="t('settings.showStatusBar')"><a-switch v-model:checked="settings.layout.showStatusBar" /></a-form-item>
          <a-form-item :label="t('settings.compactMode')"><a-switch v-model:checked="settings.layout.compactMode" /></a-form-item>
        </a-form>
      </a-tab-pane>

      <a-tab-pane key="appearance" :tab="t('settings.group.appearance')">
        <a-form layout="vertical">
          <a-form-item :label="t('settings.theme')"><a-select v-model:value="settings.theme" :options="themeOptions" /></a-form-item>
        </a-form>
      </a-tab-pane>

      <a-tab-pane key="viewer" :tab="t('settings.group.viewer')">
        <a-form layout="vertical">
          <a-form-item :label="t('settings.defaultZoomMode')">
            <a-select v-model:value="settings.viewer.defaultZoomMode" :options="zoomOptions" />
          </a-form-item>
          <a-form-item :label="t('settings.zoomStep')">
            <a-input-number v-model:value="settings.viewer.zoomStep" :min="0.01" :max="1" :step="0.01" />
          </a-form-item>
          <a-form-item :label="t('settings.smoothZoom')"><a-switch v-model:checked="settings.viewer.smoothZoom" /></a-form-item>
          <a-form-item :label="t('settings.zoomToCursor')"><a-switch v-model:checked="settings.viewer.zoomToCursor" /></a-form-item>
          <a-form-item :label="t('settings.resetZoomOnSwitch')"><a-switch v-model:checked="settings.viewer.resetZoomOnSwitch" /></a-form-item>
        </a-form>
      </a-tab-pane>

      <a-tab-pane key="large-image" :tab="t('settings.group.largeImage')">
        <a-form layout="vertical">
          <div class="settings-grid">
            <a-form-item :label="t('settings.fileSizeThresholdMB')"><a-input-number v-model:value="settings.largeImage.fileSizeThresholdMB" :min="1" /></a-form-item>
            <a-form-item :label="t('settings.pixelThreshold')"><a-input-number v-model:value="settings.largeImage.pixelThreshold" :min="1" /></a-form-item>
            <a-form-item :label="t('settings.sideThreshold')"><a-input-number v-model:value="settings.largeImage.sideThreshold" :min="1" /></a-form-item>
            <a-form-item :label="t('settings.previewMaxSize')"><a-select v-model:value="settings.largeImage.previewMaxSize" :options="[2048, 4096, 8192].map(value => ({ value, label: String(value) }))" /></a-form-item>
            <a-form-item :label="t('settings.tileSize')"><a-select v-model:value="settings.largeImage.tileSize" :options="[256, 512, 1024].map(value => ({ value, label: String(value) }))" /></a-form-item>
            <a-form-item :label="t('settings.prefetchRadius')"><a-input-number v-model:value="settings.largeImage.prefetchRadius" :min="0" /></a-form-item>
          </div>
          <a-form-item :label="t('settings.enableTilePrefetch')"><a-switch v-model:checked="settings.largeImage.enableTilePrefetch" /></a-form-item>
        </a-form>
      </a-tab-pane>

      <a-tab-pane key="thumbnail" :tab="t('settings.group.thumbnail')">
        <a-form layout="vertical">
          <a-form-item :label="t('settings.thumbnailPosition')">
            <a-select v-model:value="settings.layout.thumbnailPosition" :options="thumbnailPositionOptions" />
          </a-form-item>
          <a-form-item :label="t('settings.showThumbnailBar')"><a-switch v-model:checked="settings.layout.showThumbnailBar" /></a-form-item>
        </a-form>
      </a-tab-pane>

      <a-tab-pane key="cache" :tab="t('settings.group.cache')">
        <a-form layout="vertical">
          <div class="settings-grid">
            <a-form-item :label="t('settings.memoryCacheLimitMB')"><a-input-number v-model:value="settings.cache.memoryCacheLimitMB" :min="0" /></a-form-item>
            <a-form-item :label="t('settings.diskCacheLimitMB')"><a-input-number v-model:value="settings.cache.diskCacheLimitMB" :min="0" /></a-form-item>
          </div>
          <a-form-item :label="t('settings.enableDiskCache')"><a-switch v-model:checked="settings.cache.enableDiskCache" /></a-form-item>
          <a-form-item :label="t('settings.clearTempTileOnExit')"><a-switch v-model:checked="settings.cache.clearTempTileOnExit" /></a-form-item>
        </a-form>
      </a-tab-pane>

      <a-tab-pane key="performance" :tab="t('settings.group.performance')">
        <a-form layout="vertical">
          <div class="settings-grid">
            <a-form-item :label="t('settings.tileConcurrency')"><a-input-number v-model:value="settings.performance.tileConcurrency" :min="1" /></a-form-item>
            <a-form-item :label="t('settings.decodeConcurrency')"><a-input-number v-model:value="settings.performance.decodeConcurrency" :min="1" /></a-form-item>
            <a-form-item :label="t('settings.thumbnailConcurrency')"><a-input-number v-model:value="settings.performance.thumbnailConcurrency" :min="1" /></a-form-item>
            <a-form-item :label="t('settings.preloadNormalCount')"><a-input-number v-model:value="settings.performance.preloadNormalCount" :min="0" /></a-form-item>
            <a-form-item :label="t('settings.preloadLargePreviewCount')"><a-input-number v-model:value="settings.performance.preloadLargePreviewCount" :min="0" /></a-form-item>
          </div>
        </a-form>
      </a-tab-pane>

      <a-tab-pane key="shortcuts" :tab="t('settings.group.shortcuts')">
        <a-empty :description="t('settings.shortcutsDescription')" />
      </a-tab-pane>

      <a-tab-pane key="language" :tab="t('settings.group.language')">
        <a-form layout="vertical">
          <a-form-item :label="t('settings.language')"><a-select v-model:value="settings.language" :options="languageOptions" /></a-form-item>
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
          <a-button @click="appStore.closeSettings">{{ t('action.cancel') }}</a-button>
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
