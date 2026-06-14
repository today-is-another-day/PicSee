<script setup lang="ts">
import { computed, onBeforeUnmount, ref, toRaw, watch } from 'vue'
import { message } from 'ant-design-vue'
import { storeToRefs } from 'pinia'
import { useI18n } from 'vue-i18n'

import { useAppStore } from '@/stores/app'
import { DEFAULT_SETTINGS, useSettingsStore } from '@/stores/settings'
import type { AppSettings } from '@/types/settings'
import {
  DEFAULT_SHORTCUTS,
  eventToChord,
  findConflicts,
  formatChord,
  type ActionId,
} from '@/utils/shortcuts'

const { t } = useI18n()
const appStore = useAppStore()
const settingsStore = useSettingsStore()
const { settings, saving } = storeToRefs(settingsStore)
const draft = ref<AppSettings>(structuredClone(toRaw(settings.value)))
const capturingAction = ref<ActionId | null>(null)
const isMac = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform)

const shortcutGroups: Array<{ key: string, actions: ActionId[] }> = [
  { key: 'general', actions: ['openFile', 'openDirectory', 'settings', 'fullscreen'] },
  { key: 'browse', actions: ['previous', 'next'] },
  { key: 'view', actions: ['zoomIn', 'zoomOut', 'fitWindow', 'actualSize', 'rotateClockwise', 'rotateCounterClockwise'] },
  { key: 'file', actions: ['reveal', 'copyFile', 'delete'] },
]
const shortcutConflicts = computed(() => findConflicts(draft.value.shortcuts))
const conflictingActions = computed(() => new Set(Object.values(shortcutConflicts.value).flat()))
const hasShortcutConflicts = computed(() => Object.keys(shortcutConflicts.value).length > 0)

watch(
  () => appStore.settingsVisible,
  () => {
    stopCapture()
    draft.value = structuredClone(toRaw(settings.value))
  },
)

const maxThreads = typeof navigator !== 'undefined' && navigator.hardwareConcurrency
  ? navigator.hardwareConcurrency
  : 16
const recommendedThreads = Math.min(8, maxThreads)
const cpuThreadsHint = computed(() => t('settings.cpuThreadsHint', { recommended: recommendedThreads, max: maxThreads }))

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
const viewerBackgroundOptions = computed(() => [
  { value: 'dark', label: t('option.backgroundDark') },
  { value: 'light', label: t('option.backgroundLight') },
  { value: 'checkerboard', label: t('option.backgroundCheckerboard') },
  { value: 'custom', label: t('option.backgroundCustom') },
])

async function handleSave() {
  if (hasShortcutConflicts.value) return
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

function resetShortcuts() {
  stopCapture()
  draft.value.shortcuts = { ...DEFAULT_SHORTCUTS }
}

function startCapture(action: ActionId) {
  if (capturingAction.value === action) {
    stopCapture()
    return
  }
  stopCapture()
  capturingAction.value = action
  window.addEventListener('keydown', handleShortcutCapture, true)
}

function stopCapture() {
  capturingAction.value = null
  window.removeEventListener('keydown', handleShortcutCapture, true)
}

function handleShortcutCapture(event: KeyboardEvent) {
  event.preventDefault()
  event.stopPropagation()
  if (event.code === 'Escape') {
    stopCapture()
    return
  }
  const chord = eventToChord(event)
  if (!chord || !capturingAction.value) return
  draft.value.shortcuts[capturingAction.value] = chord
  stopCapture()
}

function handleCancel() {
  stopCapture()
  appStore.closeSettings()
}

onBeforeUnmount(stopCapture)
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
          <a-form-item :label="t('settings.viewerBackground')"><a-select v-model:value="draft.viewer.viewerBackground" :options="viewerBackgroundOptions" /></a-form-item>
          <a-form-item v-if="draft.viewer.viewerBackground === 'custom'" :label="t('settings.viewerBackgroundColor')"><a-input v-model:value="draft.viewer.viewerBackgroundColor" type="color" /></a-form-item>
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
          <a-form-item :label="t('settings.cpuThreads')" :extra="cpuThreadsHint">
            <a-input-number v-model:value="draft.performance.cpuThreads" :min="1" />
          </a-form-item>
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
        <div class="shortcuts-panel">
          <p class="settings-description">{{ t('settings.shortcutsDescription') }}</p>
          <div v-for="group in shortcutGroups" :key="group.key" class="shortcut-group">
            <h3 class="shortcut-group__title">{{ t(`settings.shortcutGroup.${group.key}`) }}</h3>
            <div
              v-for="action in group.actions"
              :key="action"
              class="shortcut-row"
              :class="{ 'shortcut-row--conflict': conflictingActions.has(action) }"
            >
              <div>
                <div class="shortcut-row__label">{{ t(`settings.shortcutAction.${action}`) }}</div>
                <div v-if="conflictingActions.has(action)" class="shortcut-row__conflict">
                  ⚠ {{ t('settings.shortcutConflict') }}
                </div>
              </div>
              <a-button
                class="shortcut-row__button"
                :danger="conflictingActions.has(action)"
                @click="startCapture(action)"
                @blur="capturingAction === action && stopCapture()"
              >
                {{ capturingAction === action ? t('settings.shortcutCapture') : formatChord(draft.shortcuts[action], isMac) }}
              </a-button>
            </div>
          </div>
          <div class="shortcut-actions">
            <a-button @click="resetShortcuts">{{ t('settings.resetShortcuts') }}</a-button>
            <span v-if="hasShortcutConflicts" class="shortcut-save-warning">
              ⚠ {{ t('settings.shortcutSaveBlocked') }}
            </span>
          </div>
        </div>
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
          <a-button type="primary" :loading="saving" :disabled="hasShortcutConflicts" @click="handleSave">{{ t('action.save') }}</a-button>
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

.shortcut-group {
  margin-top: 18px;
}

.shortcut-group__title {
  margin: 0 0 8px;
  color: var(--muted-color);
  font-size: 12px;
  text-transform: uppercase;
}

.shortcut-row {
  display: flex;
  min-height: 44px;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  border-bottom: 1px solid var(--border-color);
}

.shortcut-row__conflict,
.shortcut-save-warning {
  color: var(--ant-color-error, #ff4d4f);
  font-size: 12px;
}

.shortcut-row__button {
  min-width: 136px;
}

.shortcut-actions {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-top: 20px;
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
