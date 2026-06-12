import { createApp } from 'vue'
import { createPinia } from 'pinia'
import Antd from 'ant-design-vue'
import 'ant-design-vue/dist/reset.css'

import App from './App.vue'
import { i18n } from './i18n'
import './styles/main.css'

createApp(App).use(createPinia()).use(i18n).use(Antd).mount('#app')
