import './style.css'
window.__errors && window.__errors.push('main-js-ran')
import './style.css'
import { createApp } from 'vue'
import App from './App.vue'

const app = createApp(App)
app.config.errorHandler = (err, _inst, info) => {
  const text = `VueError: ${err && err.message ? err.message : err} (${info})`
  window.__errors && window.__errors.push(text)
  try { window.__showBanner(text) } catch (_) {}
}
app.mount('#app')
window.__errors && window.__errors.push('vue-mounted')
window.__diagFlush && window.__diagFlush()
window.__diagFlush && window.__diagFlush()
