import { defineConfig, loadEnv } from 'vite';
import { fusionApi } from './scripts/fusion-api.ts';

export default defineConfig(({ mode }) => {
  const env = { ...loadEnv(mode, process.cwd(), ''), ...process.env };
  return {
    plugins: [fusionApi(env.FUSION_API_URL ?? '', env.FUSION_API_KEY ?? '')],
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
