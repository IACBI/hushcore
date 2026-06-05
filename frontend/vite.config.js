import { defineConfig } from 'vite';

export default defineConfig({
  root: '.',
  build: {
    outDir: '../backend/static',
    emptyOutDir: true,
  },
  server: {
    port: 3000,
    host: '127.0.0.1',
  }
});
