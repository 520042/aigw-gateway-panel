import { defineConfig, type Plugin } from 'vite';
import react from '@vitejs/plugin-react';
import { renameSync, existsSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = fileURLToPath(new URL('.', import.meta.url));

/** 把构建出的 index.html 从 web/app/ 搬到 web/（rust-embed 服务的是 web/index.html）。 */
function moveHtml(): Plugin {
  return {
    name: 'aigw-move-html',
    apply: 'build',
    closeBundle() {
      const from = resolve(ROOT, '../web/app/index.html');
      const to = resolve(ROOT, '../web/index.html');
      if (existsSync(from)) renameSync(from, to);
    },
  };
}

export default defineConfig({
  plugins: [react(), moveHtml()],
  // 产物在 web/app/ 下，但页面从 web/index.html 提供；资源 URL 必须带 /app/ 前缀，
  // 否则浏览器会去请求 /app.js（那是旧版脚本，已废弃）。
  base: '/app/',
  build: {
    outDir: '../web/app',
    emptyOutDir: true,
    assetsDir: 'assets',
    rollupOptions: {
      output: {
        entryFileNames: 'app.js',
        assetFileNames: (a) => (a.name?.endsWith('.css') ? 'app.css' : 'assets/[name]-[hash][extname]'),
      },
    },
  },
});
