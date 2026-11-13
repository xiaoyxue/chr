import { join } from 'node:path';
import { defineConfig, loadEnv, type ConfigEnv, type UserConfig } from 'vite';
import { fusionApi } from './scripts/fusion-api.ts';
import { localFiles } from './scripts/local-files.ts';

export default defineConfig(({ mode }) => {
  const env = { ...loadEnv(mode, process.cwd(), ''), ...process.env };
  return {
    plugins: [
      fusionApi(env.FUSION_API_URL ?? '', env.FUSION_API_KEY ?? ''),
      localFiles(env.LOCAL_FILES_ROOTS ?? ''),
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
      strictPort: true,
      cors: false,
      fs: { deny: ['.env', '.env.*', '*.{crt,pem}', '**/.git/**', '**/.local-data/**'] },
    },
    preview: { host: '127.0.0.1', cors: false },
  };
});
