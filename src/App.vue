<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, shallowRef, watch } from 'vue'
import { ConfigProvider, theme as antTheme } from 'ant-design-vue'
import { storeToRefs } from 'pinia'

import AppLayout from '@/components/AppLayout.vue'
import { i18n } from '@/i18n'
import { useSettingsStore } from '@/stores/settings'

const settingsStore = useSettingsStore()
const { settings } = storeToRefs(settingsStore)
const darkModeQuery = window.matchMedia('(prefers-color-scheme: dark)')
const systemDark = shallowRef(darkModeQuery.matches)
const systemLanguage: 'zh-CN' | 'en-US' = navigator.language.toLowerCase().startsWith('zh') ? 'zh-CN' : 'en-US'

const isDark = computed(() => settings.value.theme === 'dark' || (settings.value.theme === 'system' && systemDark.value))
const locale = computed(() => settings.value.language === 'system' ? systemLanguage : settings.value.language)
const themeConfig = computed(() => ({
  algorithm: isDark.value ? antTheme.darkAlgorithm : antTheme.defaultAlgorithm,
  token: { colorPrimary: '#1677ff', borderRadius: 8 },
}))

function applyAppearance() {
  document.documentElement.dataset.theme = isDark.value ? 'dark' : 'light'
  document.documentElement.lang = locale.value
  i18n.global.locale.value = locale.value
}

function handleSystemThemeChange(event: MediaQueryListEvent) {
  systemDark.value = event.matches
}

watch([isDark, locale], applyAppearance, { immediate: true })
onMounted(() => {
  darkModeQuery.addEventListener('change', handleSystemThemeChange)
  void settingsStore.loadSettings()
})
onBeforeUnmount(() => darkModeQuery.removeEventListener('change', handleSystemThemeChange))
</script>

<template>
  <ConfigProvider :theme="themeConfig">
    <AppLayout />
  </ConfigProvider>
</template>
