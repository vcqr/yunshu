import './style.css'
import { initTheme } from './lib/theme'
import { createApp } from 'vue'
import App from './App.vue'

initTheme()

const app = createApp(App)
app.config.errorHandler = (err, _inst, info) => {
  console.error('VueError:', err, info)
}
app.mount('#app')
