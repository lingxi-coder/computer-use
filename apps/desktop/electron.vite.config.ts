import { resolve } from 'node:path';

import react from '@vitejs/plugin-react';
import { defineConfig, externalizeDepsPlugin } from 'electron-vite';

// The workspace packages are TypeScript sources: they are bundled, not left as
// requires that Node could not load. Real third-party dependencies stay external.
const bundled = ['@computer-user/desktop-api', '@computer-user/decision-worker'];

export default defineConfig({
  main: {
    plugins: [externalizeDepsPlugin({ exclude: bundled })],
    build: {
      rollupOptions: {
        input: { index: resolve(__dirname, 'src/main/index.ts') },
      },
    },
  },
  preload: {
    // A sandboxed preload can only require Electron's small allowlist, so it is one bundle.
    plugins: [externalizeDepsPlugin({ exclude: bundled })],
    build: {
      rollupOptions: {
        input: { index: resolve(__dirname, 'src/preload/index.ts') },
        output: { format: 'cjs', entryFileNames: '[name].cjs' },
      },
    },
  },
  renderer: {
    root: resolve(__dirname, 'src/renderer'),
    build: {
      rollupOptions: {
        input: { index: resolve(__dirname, 'src/renderer/index.html') },
      },
    },
    resolve: { alias: { '@renderer': resolve(__dirname, 'src/renderer') } },
    plugins: [react()],
  },
});
