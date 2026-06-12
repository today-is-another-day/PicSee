<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, shallowRef, watch } from 'vue'
import { ConfigProvider, message, theme as antTheme } from 'ant-design-vue'
import enUS from 'ant-design-vue/es/locale/en_US'
import zhCN from 'ant-design-vue/es/locale/zh_CN'
import { storeToRefs } from 'pinia'

import AppLayout from '@/components/AppLayout.vue'
import { i18n } from '@/i18n'
import { useSettingsStore } from '@/stores/settings'

const settingsStore = useSettingsStore()
const { loadError, settings } = storeToRefs(settingsStore)
const darkModeQuery = window.matchMedia('(prefers-color-scheme: dark)')
const systemDark = shallowRef(darkModeQuery.matches)
const systemLanguage = shallowRef(getSystemLanguage())

const isDark = computed(() => settings.value.theme === 'dark' || (settings.value.theme === 'system' && systemDark.value))
const locale = computed(() => settings.value.language === 'system' ? systemLanguage.value : settings.value.language)
const antLocale = computed(() => locale.value === 'zh-CN' ? zhCN : enUS)
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

function getSystemLanguage(): 'zh-CN' | 'en-US' {
  return navigator.language.toLowerCase().startsWith('zh') ? 'zh-CN' : 'en-US'
}

function handleSystemLanguageChange() {
  systemLanguage.value = getSystemLanguage()
}

watch([isDark, locale], applyAppearance, { immediate: true })
watch(loadError, (error) => {
  if (error) void message.warning(i18n.global.t('settings.loadFailed'))
})
onMounted(() => {
  darkModeQuery.addEventListener('change', handleSystemThemeChange)
  window.addEventListener('languagechange', handleSystemLanguageChange)
  void settingsStore.loadSettings()
})
onBeforeUnmount(() => {
  darkModeQuery.removeEventListener('change', handleSystemThemeChange)
  window.removeEventListener('languagechange', handleSystemLanguageChange)
})
</script>

<template>
  <ConfigProvider :locale="antLocale" :theme="themeConfig">
    <AppLayout />
  </ConfigProvider>
</template>
