import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import { viteSingleFile } from 'vite-plugin-singlefile'
import { resolve } from 'path'

export default defineConfig({
  plugins: [vue(), viteSingleFile()],
  resolve: {
    alias: { '@': resolve(__dirname, 'src') },
  },
  build: { target: 'esnext' },
  server: {
    proxy: { '/api': 'http://127.0.0.1:7681' },
  },
})
