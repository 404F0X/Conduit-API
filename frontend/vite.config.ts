import path from 'path';
import { defineConfig, type Plugin } from 'vite';
import react from '@vitejs/plugin-react-swc';
import tailwindcss from '@tailwindcss/vite';
import tanstackRouter from '@tanstack/router-plugin/vite';
import { copyFile, mkdir } from 'node:fs/promises';

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

function includeLegalNotices(): Plugin {
  return {
    name: 'conduit-legal-notices',
    apply: 'build',
    async writeBundle(options) {
      const configuredOutput = options.dir ?? 'dist';
      const outputDirectory = path.isAbsolute(configuredOutput) ? configuredOutput : path.resolve(__dirname, configuredOutput);
      const licenseDirectory = path.join(outputDirectory, 'licenses');
      const frontendNoticeDirectory = path.join(licenseDirectory, 'frontend');
      const licenseTextsDirectory = path.join(licenseDirectory, 'LICENSES');
      await Promise.all([mkdir(frontendNoticeDirectory, { recursive: true }), mkdir(licenseTextsDirectory, { recursive: true })]);
      await Promise.all([
        copyFile(path.resolve(__dirname, '../LICENSE'), path.join(licenseDirectory, 'LICENSE')),
        copyFile(path.resolve(__dirname, '../NOTICE'), path.join(licenseDirectory, 'NOTICE')),
        copyFile(path.resolve(__dirname, '../LICENSING.md'), path.join(licenseDirectory, 'LICENSING.md')),
        copyFile(path.resolve(__dirname, '../RELINKING.md'), path.join(licenseDirectory, 'RELINKING.md')),
        copyFile(path.resolve(__dirname, '../LICENSES/LGPL-3.0-only.txt'), path.join(licenseTextsDirectory, 'LGPL-3.0-only.txt')),
        copyFile(path.resolve(__dirname, 'NOTICE'), path.join(frontendNoticeDirectory, 'NOTICE')),
      ]);
    },
  };
}

// https://vite.dev/config/
export default defineConfig({
  // The production server injects a document <base> at runtime. Relative
  // chunk URLs let the same build work at both / and server.base_path.
  base: './',
  build: {
    // Keep the license texts for dependencies that Rollup actually bundles.
    // This avoids both an incomplete hand-maintained list and notices for
    // development-only packages that never ship to users.
    license: {
      fileName: 'licenses/frontend/THIRD_PARTY_LICENSES.md',
    },
  },
  plugins: [
    tanstackRouter({
      target: 'react',
      autoCodeSplitting: true,
    }),
    react(),
    tailwindcss(),
    enforceJavaScriptChunkBudget(),
    includeLegalNotices(),
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
