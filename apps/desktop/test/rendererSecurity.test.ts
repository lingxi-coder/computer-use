import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import react from '@vitejs/plugin-react';
import { createServer } from 'vite';

import {
  contentSecurityPolicyForUrl,
  DEVELOPMENT_CSP_NONCE,
  developmentCspPlugin,
  developmentRendererUrl,
  PRODUCTION_CONTENT_SECURITY_POLICY,
} from '../src/main/rendererSecurity.js';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../src/renderer');

test('development URLs must be loopback HTTP and are unavailable in a packaged app', () => {
  for (const url of ['http://localhost:5173', 'http://127.0.0.1:5173', 'https://localhost:5173']) {
    assert.equal(developmentRendererUrl(url, false), new URL(url).toString());
    assert.equal(developmentRendererUrl(url, true), undefined);
  }
  for (const url of [undefined, '', 'bad', 'file:///tmp/index.html', 'https://example.com', 'http://localhost.evil', 'http://user:secret@localhost', 'https://[::1]:5173']) {
    assert.equal(developmentRendererUrl(url, false), undefined);
  }
});

test('main authorizes the development nonce and only that origin’s HMR endpoint', () => {
  const dev = 'http://127.0.0.1:5173';
  const policy = contentSecurityPolicyForUrl(dev + '/index.html', dev);
  assert.match(policy, new RegExp(`script-src 'self' 'nonce-${DEVELOPMENT_CSP_NONCE}'`));
  assert.match(policy, new RegExp(`style-src 'self' 'nonce-${DEVELOPMENT_CSP_NONCE}'`));
  assert.match(policy, /connect-src 'self' ws:\/\/127\.0\.0\.1:5173/);
  assert.equal(policy.includes('unsafe-inline'), false);
  for (const requested of ['file:///app/index.html', 'http://127.0.0.1:9999', 'https://example.com', 'bad']) {
    assert.equal(contentSecurityPolicyForUrl(requested, dev), PRODUCTION_CONTENT_SECURITY_POLICY);
  }
  assert.equal(contentSecurityPolicyForUrl('https://example.com', 'https://example.com'), PRODUCTION_CONTENT_SECURITY_POLICY);
  assert.match(contentSecurityPolicyForUrl('https://localhost:5173/', 'https://localhost:5173'), /wss:\/\/localhost:5173/);
});

test('Vite’s actual React preamble receives the nonce allowed by both development CSPs', async () => {
  const source = readFileSync(resolve(root, 'index.html'), 'utf8');
  assert.ok(source.includes(`content="${PRODUCTION_CONTENT_SECURITY_POLICY}"`), 'the packaged HTML keeps strict CSP');
  assert.equal(developmentCspPlugin().apply, 'serve', 'production builds cannot relax the HTML policy');
  const server = await createServer({
    configFile: false,
    root,
    plugins: [react(), developmentCspPlugin()],
    optimizeDeps: { noDiscovery: true, include: [] },
    server: { middlewareMode: true, ws: false },
  });
  try {
    const html = await server.transformIndexHtml('/', source);
    const preamble = html.match(/<script[^>]*>import \{ injectIntoGlobalHook \}[^<]*<\/script>/)?.[0];
    assert.ok(preamble, 'the React plugin really injected its inline initialization');
    assert.ok(preamble.includes(`nonce="${DEVELOPMENT_CSP_NONCE}"`));
    const metaPolicy = html.match(/http-equiv="Content-Security-Policy" content="([^"]+)"/)?.[1];
    assert.ok(metaPolicy, 'the development page includes its CSP');
    assert.ok(metaPolicy.includes(`'nonce-${DEVELOPMENT_CSP_NONCE}'`), 'meta CSP authorizes the same preamble');
    assert.ok(metaPolicy.includes('ws://127.0.0.1:*'), 'meta CSP allows the local HMR socket');
    assert.ok(html.includes(`property="csp-nonce" nonce="${DEVELOPMENT_CSP_NONCE}"`), 'Vite exposes the nonce for injected styles');
    const app = await server.transformRequest('/App.tsx');
    assert.ok(app?.code.includes("can't detect preamble"), 'this is the Fast Refresh path that previously failed');
  } finally {
    await server.close();
  }
});
