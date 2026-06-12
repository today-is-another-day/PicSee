<script setup lang="ts">
import { useI18n } from 'vue-i18n'

import { useAppStore } from '@/stores/app'

const { t } = useI18n()
const appStore = useAppStore()
</script>

<template>
  <header class="top-toolbar">
    <div class="top-toolbar__brand">
      <span class="top-toolbar__logo">P</span>
      <div>
        <strong>{{ t('app.name') }}</strong>
        <span>{{ t('app.subtitle') }}</span>
      </div>
    </div>
    <div class="top-toolbar__actions">
      <a-button>{{ t('action.openFile') }}</a-button>
      <a-button>{{ t('action.openDirectory') }}</a-button>
      <a-button-group>
        <a-button :aria-label="t('action.previous')">{{ t('action.previous') }}</a-button>
        <a-button :aria-label="t('action.next')">{{ t('action.next') }}</a-button>
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
  display: grid;
  width: 34px;
  height: 34px;
  place-items: center;
  border-radius: 10px;
  background: #1677ff;
  color: white !important;
  font-size: 18px !important;
  font-weight: 700;
}
</style>
