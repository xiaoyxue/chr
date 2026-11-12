import { Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import type { Connect, Plugin } from 'vite';

export function fusionMiddleware(baseUrl: string, apiKey: string): Connect.NextHandleFunction {
  return (req, res, next) => {
    const url = new URL(req.url, 'http://localhost');
    if (!url.pathname.startsWith('/fusion-api/')) return next();
    const headers = { 'Cache-Control': 'private, no-store', 'X-Content-Type-Options': 'nosniff' };
    const fail = (status: number, error: string) => {
      res.writeHead(status, { ...headers, 'Content-Type': 'application/json' }).end(JSON.stringify({ error }));
    };
    if (req.method !== 'GET') return fail(405, 'Only GET is supported.');
    const route = /^\/fusion-api\/matches\/([A-Za-z0-9][A-Za-z0-9_-]{0,63})(?:\/assets\/([A-Za-z0-9][A-Za-z0-9_-]{0,63}))?$/.exec(url.pathname);
    if (!route || url.search) return fail(400, 'Invalid Fusion resource path.');
    if (!baseUrl || !apiKey) return fail(503, 'Configure FUSION_API_URL and FUSION_API_KEY on the onsite server.');
    const abort = new AbortController();
    const timer = setTimeout(() => abort.abort(), 120000);
    res.on('close', () => { clearTimeout(timer); abort.abort(); });
    void (async () => {
      const base = new URL(baseUrl);
      if (!['http:', 'https:'].includes(base.protocol) || base.username || base.password)
        throw new Error('Invalid Fusion server configuration.');
      const path = `/api/LiveIntegration/Matches/${route[1]}${route[2] ? `/Assets/${route[2]}` : ''}`;
      const upstream = await fetch(new URL(path, base), {
        headers: { 'X-Fusion-Api-Key': apiKey }, signal: abort.signal, redirect: 'error',
      });
      if (!upstream.ok) {
        await upstream.body?.cancel();
        fail(upstream.status, upstream.status === 404 ? 'Fusion match or assigned asset not found.'
          : upstream.status === 401 ? 'The onsite server API key was rejected by Fusion.'
          : `Fusion request failed (${upstream.status}).`);
        return;
      }
      res.writeHead(200, {
        ...headers,
        'Content-Type': upstream.headers.get('content-type') ?? 'application/octet-stream',
      });
      if (!upstream.body) throw new Error('Fusion returned an empty response.');
      const reader = upstream.body.getReader();
      const stream = Readable.from((async function* () {
        try {
          while (true) {
            const result = await reader.read();
            if (result.done) break;
            yield result.value;
          }
        } finally { reader.releaseLock(); }
      })());
      await pipeline(stream, res);
    })().catch(() => {
      if (res.destroyed) return;
      if (res.headersSent) res.destroy();
      else fail(502, 'The onsite server could not reach Fusion. Check its URL and connection.');
    }).finally(() => clearTimeout(timer));
  };
}

export function fusionApi(baseUrl: string, apiKey: string): Plugin {
  return {
    name: 'fusion-read-only-api',
    configureServer(server) { server.middlewares.use(fusionMiddleware(baseUrl, apiKey)); },
    configurePreviewServer(server) { server.middlewares.use(fusionMiddleware(baseUrl, apiKey)); },
  };
}
