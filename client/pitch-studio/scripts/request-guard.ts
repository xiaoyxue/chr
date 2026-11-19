import type { IncomingMessage } from 'node:http';

/** A refused request: its HTTP status and a message for the page. */
export class Failure extends Error {
  constructor(readonly status: number, message: string, readonly extra: Record<string, unknown> = {}) { super(message); }
}

// The page server answers loopback hosts only, but that does not stop another website open in
// the operator's browser from sending to 127.0.0.1 itself (CSRF). A request that changes data
// must therefore come from the studio page's own origin, and carry a body type a cross-site
// form cannot send without a CORS preflight.

/** Why a state-changing request is refused, or undefined when it comes from the studio page. */
export function writeProblem(req: IncomingMessage, contentType?: string): { status: number; message: string } | undefined {
  const origin = req.headers.origin, site = req.headers['sec-fetch-site'];
  const sameSite = origin ? URL.canParse(origin) && new URL(origin).host === req.headers.host : site === 'same-origin' || site === 'same-site';
  if (!sameSite) return { status: 403, message: 'Changes are accepted from the studio page only.' };
  const type = (req.headers['content-type'] ?? '').split(';')[0].trim().toLowerCase();
  if (contentType && type !== contentType) return { status: 415, message: `Expected Content-Type ${contentType}.` };
  return undefined;
}
