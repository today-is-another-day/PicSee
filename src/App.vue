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

  // TODO M4-debug：开发期端到端自动验证（DEV only）。
  // 触发方式：
  //   1. 启动时设环境变量 VITE_PICSEE_AUTOOPEN=<abs path>
  //   2. sessionStorage.setItem('__picseeAutoOpen', '<abs path>') 后刷新
  //   3. DevTools 调用 window.__picseeDebugOpen('<abs path>')
  if (import.meta.env.DEV) {
    const autoPath = sessionStorage.getItem('__picseeAutoOpen')
      ?? (import.meta.env.VITE_PICSEE_AUTOOPEN as string | undefined)
      ?? null
    if (autoPath) {
      // 轮询等待 main.ts 注入的 __picseeDebugOpen（动态 import 存在竞态）
      const tryOpen = (attempt = 0) => {
        const w = window as unknown as { __picseeDebugOpen?: (p: string) => Promise<void> }
        if (w.__picseeDebugOpen) {
          void (async () => {
            await w.__picseeDebugOpen!(autoPath)
            // 放大到 actual-size（zoom=1 > previewScale）以触发 tile 加载路径
            const { useViewerStore } = await import('@/stores/viewer')
            setTimeout(() => useViewerStore().applyDisplayMode('actual-size'), 800)
          })()
        }
        else if (attempt < 40) {
          setTimeout(() => tryOpen(attempt + 1), 100)
        }
      }
      setTimeout(() => tryOpen(), 600)
    }
  }
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
