import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

export default defineConfig({
  plugins: [
    vue(),
    // TODO M4-debug：开发期接收前端 E2E 测试结果的临时 endpoint，后续移除
    {
      name: 'picsee-e2e-result',
      configureServer(server) {
        server.middlewares.use('/__picsee_e2e_result', (req, res, next) => {
          if (req.method !== 'POST') { next(); return }
          let body = ''
          req.on('data', (chunk: Buffer) => { body += chunk.toString() })
          req.on('end', () => {
            try {
              const data: unknown = JSON.parse(body)
              process.stdout.write(`\n[PicSee-E2E] 结果: ${JSON.stringify(data)}\n`)
            }
            catch {
              process.stdout.write(`\n[PicSee-E2E] 原始结果: ${body}\n`)
            }
            res.writeHead(204)
            res.end()
          })
        })
      },
    },
  ],
  resolve: {
    alias: {
      '@': new URL('./src', import.meta.url).pathname,
    },
  },
  clearScreen: false,
  server: {
    host: '127.0.0.1',
    port: 1420,
    strictPort: true,
  },
})
