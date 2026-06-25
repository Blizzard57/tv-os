import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// In development the Vite dev server proxies API calls to a locally running
// tvosd (`cargo run` in ../tvosd). In production tvosd serves the built UI
// itself, so no proxy is involved.
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      '/api': 'http://127.0.0.1:8484',
    },
  },
});
