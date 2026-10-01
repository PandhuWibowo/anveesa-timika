import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'

// Dev server proxies the API to the Rust backend on :8200 so the SPA calls
// same-origin paths: gRPC-Web under `/timika.v1.*`, plus `/v1` for the health
// check and the web-terminal WebSocket.
export default defineConfig({
  plugins: [svelte()],
  server: {
    port: 5174,
    proxy: {
      '/v1': { target: 'http://127.0.0.1:8200', changeOrigin: true, ws: true },
      '/timika.v1.': { target: 'http://127.0.0.1:8200', changeOrigin: true },
    },
  },
  preview: {
    port: 4174,
    proxy: {
      '/v1': { target: 'http://127.0.0.1:8200', changeOrigin: true, ws: true },
      '/timika.v1.': { target: 'http://127.0.0.1:8200', changeOrigin: true },
    },
  },
})
