import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { defineConfig, loadEnv, type ConfigEnv, type Connect, type Plugin, type UserConfig } from 'vite';
import { fusionApi } from './scripts/fusion-api.ts';
import { localFiles } from './scripts/local-files.ts';
import { studioStore } from './scripts/studio-store.ts';

// Without STUDIO_DATA_DIR the studio keeps its data in the repository's ignored output/.
const DEFAULT_STUDIO_DATA_DIR = fileURLToPath(new URL('../../output/pitch-studio', import.meta.url));

// The OpenEXR panoramas 场景灯光 offers: the asset/ibl submodule folder by default.
const DEFAULT_IBL_DIR = fileURLToPath(new URL('../../asset/ibl', import.meta.url));

// Tells the page server-side settings it cannot discover itself.
function studioConfig(iblDir: string): Plugin {
  const middleware: Connect.NextHandleFunction = (request, response, next) => {
    if (request.url !== '/studio-config') { next(); return; }
    response.setHeader('Content-Type', 'application/json');
    response.setHeader('Cache-Control', 'no-store');
    response.end(JSON.stringify({ iblDir }));
  };
  return {
    name: 'studio-config',
    configureServer(server) { server.middlewares.use(middleware); },
    configurePreviewServer(server) { server.middlewares.use(middleware); },
  };
}

// The page reaches chr-server's control WebSocket through its own origin, so one
// tunnel to this page is enough. chr-server accepts the forwarded loopback origin.
const DEFAULT_RENDER_URL = 'ws://127.0.0.1:8787';
function renderProxy(url = DEFAULT_RENDER_URL) {
  return { '/chr-render': { target: url, ws: true, rewrite: () => '/' } };
}

// Tells the page where /chr-render forwards to, so a failed connection can name it.
function renderTarget(url = DEFAULT_RENDER_URL): Plugin {
  const middleware: Connect.NextHandleFunction = (request, response, next) => {
    if (request.url !== '/chr-render-info') { next(); return; }
    response.setHeader('Content-Type', 'application/json');
    response.setHeader('Cache-Control', 'no-store');
    response.end(JSON.stringify({ target: url }));
  };
  return {
    name: 'chr-render-target',
    configureServer(server) { server.middlewares.use(middleware); },
    configurePreviewServer(server) { server.middlewares.use(middleware); },
  };
}

export default defineConfig(({ mode }) => {
  const env = { ...loadEnv(mode, process.cwd(), ''), ...process.env };
  return {
    plugins: [
      fusionApi(env.FUSION_API_URL ?? '', env.FUSION_API_KEY ?? ''),
      localFiles(env.LOCAL_FILES_ROOTS ?? '', env.STUDIO_DATA_DIR || DEFAULT_STUDIO_DATA_DIR),
      studioStore(env.STUDIO_DATA_DIR || DEFAULT_STUDIO_DATA_DIR),
      renderTarget(env.CHR_RENDER_URL),
      studioConfig(env.STUDIO_IBL_DIR || DEFAULT_IBL_DIR),
      {
        name: 'isolated-dependency-cache',
        // CLI --port is available here, after Vite merges its inline configuration.
        config(config: UserConfig, { command }: ConfigEnv) {
          return { cacheDir: join('node_modules', '.vite', `${command}-${config.server?.port ?? 5186}`) };
        },
      },
    ],
    // Lazy worker dependencies must not trigger a reload that discards selected local files.
    optimizeDeps: {
      include: [
        'three', 'three/addons/loaders/GLTFLoader.js', 'three/addons/environments/RoomEnvironment.js',
        'three/addons/controls/OrbitControls.js', 'three/addons/loaders/PLYLoader.js',
        'hyparquet', 'hyparquet-compressors', 'fflate',
      ],
    },
    server: {
      host: '127.0.0.1',
      port: 5186,
      proxy: renderProxy(env.CHR_RENDER_URL),
      strictPort: true,
      cors: false,
      fs: { deny: ['.env', '.env.*', '*.{crt,pem}', '**/.git/**', '**/.local-data/**'] },
    },
    preview: { host: '127.0.0.1', cors: false, proxy: renderProxy(env.CHR_RENDER_URL) },
  };
});
