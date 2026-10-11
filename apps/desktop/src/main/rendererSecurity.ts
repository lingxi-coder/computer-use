import type { Plugin } from 'vite';

export const PRODUCTION_CONTENT_SECURITY_POLICY = [
  "default-src 'self'",
  "script-src 'self'",
  "style-src 'self'",
  "img-src 'self' data: blob:",
  "connect-src 'none'",
  "object-src 'none'",
  "base-uri 'none'",
  "form-action 'none'",
].join('; ');

// Used only by the local development server; packaged pages do not authorize this nonce.
export const DEVELOPMENT_CSP_NONCE = 'cu-local-development';

export function developmentRendererUrl(raw: string | undefined, isPackaged: boolean): string | undefined {
  if (isPackaged || !raw) return undefined;
  try {
    const url = new URL(raw);
    // Keep origins aligned with the WebSocket hosts accepted by the development CSP.
    const loopback = ['localhost', '127.0.0.1'].includes(url.hostname);
    if ((url.protocol === 'http:' || url.protocol === 'https:') && loopback && !url.username && !url.password) {
      return url.toString();
    }
  } catch {
    // Invalid configuration uses the packaged renderer and its strict policy.
  }
  return undefined;
}

function developmentPolicy(websocketOrigins: string[]): string {
  return PRODUCTION_CONTENT_SECURITY_POLICY
    .replace("script-src 'self'", `script-src 'self' 'nonce-${DEVELOPMENT_CSP_NONCE}'`)
    .replace("style-src 'self'", `style-src 'self' 'nonce-${DEVELOPMENT_CSP_NONCE}'`)
    .replace("connect-src 'none'", `connect-src 'self' ${websocketOrigins.join(' ')}`);
}

/** Main relaxes CSP only for responses from the validated development origin. */
export function contentSecurityPolicyForUrl(requestUrl: string, devUrl: string | undefined): string {
  const validated = developmentRendererUrl(devUrl, false);
  if (validated) {
    try {
      const origin = new URL(validated);
      if (new URL(requestUrl).origin === origin.origin) {
        origin.protocol = origin.protocol === 'https:' ? 'wss:' : 'ws:';
        return developmentPolicy([origin.origin]);
      }
    } catch {
      // Malformed requests receive the strict policy too.
    }
  }
  return PRODUCTION_CONTENT_SECURITY_POLICY;
}

/** Vite's inline React preamble and injected styles use a nonce in development. */
export function developmentCspPlugin(): Plugin {
  const websocketOrigins = ['localhost', '127.0.0.1'].flatMap((host) => [
    `ws://${host}:*`,
    `wss://${host}:*`,
  ]);
  return {
    name: 'computer-use-development-csp',
    apply: 'serve',
    config: () => ({ html: { cspNonce: DEVELOPMENT_CSP_NONCE } }),
    transformIndexHtml: {
      order: 'post',
      handler: (html) => html.replace(
        /<meta\s+http-equiv="Content-Security-Policy"\s+content="[^"]*"\s*\/?>/i,
        `<meta http-equiv="Content-Security-Policy" content="${developmentPolicy(websocketOrigins)}" />`,
      ),
    },
  };
}
