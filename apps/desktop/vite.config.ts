import { fileURLToPath, URL } from 'node:url';
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

// Tauri expects a fixed port and no clearing of its output.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  resolve: {
    alias: {
      '@locales': fileURLToPath(new URL('../../locales', import.meta.url)),
      '@icons': fileURLToPath(new URL('../../icons', import.meta.url)),
    },
  },
  server: {
    port: 1420,
    strictPort: true,
    fs: { allow: ['..', '../..'] },
    watch: { ignored: ['**/src-tauri/**'] },
  },
  envPrefix: ['VITE_', 'TAURI_ENV_'],
  build: {
    target: 'es2022',
    minify: 'oxc',
    sourcemap: false,
    chunkSizeWarningLimit: 1500,
  },
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts'],
  },
});
