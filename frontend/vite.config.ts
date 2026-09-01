import path from 'path';
import { defineConfig, type Plugin } from 'vite';
import react from '@vitejs/plugin-react-swc';
import tailwindcss from '@tailwindcss/vite';
import tanstackRouter from '@tanstack/router-plugin/vite';

const MAX_JS_CHUNK_BYTES = 1_250_000;

function enforceJavaScriptChunkBudget(): Plugin {
  return {
    name: 'conduit-javascript-chunk-budget',
    apply: 'build',
    generateBundle(_options, bundle) {
      for (const output of Object.values(bundle)) {
        if (output.type !== 'chunk') continue;

        const size = Buffer.byteLength(output.code);
        if (size > MAX_JS_CHUNK_BYTES) {
          this.error(
            `${output.fileName} is ${(size / 1_000_000).toFixed(2)} MB, exceeding the ${(MAX_JS_CHUNK_BYTES / 1_000_000).toFixed(2)} MB JavaScript chunk budget`
          );
        }
      }
    },
  };
}

// https://vite.dev/config/
export default defineConfig({
  // The production server injects a document <base> at runtime. Relative
  // chunk URLs let the same build work at both / and server.base_path.
  base: './',
  plugins: [
    tanstackRouter({
      target: 'react',
      autoCodeSplitting: true,
    }),
    react(),
    tailwindcss(),
    enforceJavaScriptChunkBudget(),
  ],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),

      // fix loading all icon chunks in dev mode
      // https://github.com/tabler/tabler-icons/issues/1233
      '@tabler/icons-react': '@tabler/icons-react/dist/esm/icons/index.mjs',
    },
  },
  server: {
    port: process.env.VITE_PORT ? parseInt(process.env.VITE_PORT) : 5173,
    proxy: {
      '/admin': {
        target: process.env.VITE_API_URL || 'http://localhost:8090',
        changeOrigin: true,
      },
      '/oauth': {
        target: process.env.VITE_API_URL || 'http://localhost:8090',
        changeOrigin: true,
        bypass: (req) => {
          if (req.url?.includes('idp-callback')) {
            return req.url;
          }
        },
      },
      '/v1': {
        target: process.env.VITE_API_URL || 'http://localhost:8090',
        changeOrigin: true,
      },
    },
  },
});
