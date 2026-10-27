import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import tailwindcss from '@tailwindcss/vite'
import { resolve } from 'node:path'

export default defineConfig({
  plugins: [vue(), tailwindcss()],
  clearScreen: false,
  // 多页：主窗口 index.html + 远程观看窗口 viewer.html
  build: {
    rollupOptions: {
      input: { main: resolve(__dirname, 'index.html'), viewer: resolve(__dirname, 'viewer.html') },
    },
  },
  server: { port: 5173, strictPort: true }
})
