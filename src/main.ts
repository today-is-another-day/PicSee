import { createApp } from 'vue'
import { createPinia } from 'pinia'
import Antd from 'ant-design-vue'
import 'ant-design-vue/dist/reset.css'

import App from './App.vue'
import { i18n } from './i18n'
import './styles/main.css'

createApp(App).use(createPinia()).use(i18n).use(Antd).mount('#app')

// TODO M4-debug：开发期调试入口，后续移除
if (import.meta.env.DEV) {
  void (async () => {
    const { useLargeImage } = await import('@/composables/useLargeImage')
    const w = window as unknown as Record<string, unknown>
    w.__picseeDebugOpen = async (path: string) => {
      const entry = {
        path,
        name: path.split('/').pop() ?? path,
        size: 0,
        modified: Date.now(),
      }
      const { openImage } = useLargeImage()
      await openImage(entry)
      console.log('[PicSee] __picseeDebugOpen complete', path)
    }
  })()
}
