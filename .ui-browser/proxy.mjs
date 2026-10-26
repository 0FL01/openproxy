// Synthetic fixture only: same-origin TLS and local fonts in front of the
// isolated OpenProxy test app. Never configure this proxy for production.
import { readFileSync, readdirSync } from 'node:fs';
import http from 'node:http';
import https from 'node:https';

const root = '/opt/ui-browser';
const fonts = new Set(readdirSync(`${root}/fonts`).filter((name) => name.endsWith('.woff2')));
const googleFonts = /@import\s*(?:url\()?\s*(["'])(https:\/\/fonts\.googleapis\.com\/css2\?[^"']*)\1\s*\)?\s*;/g;
const dashboardFonts = 'https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700&family=Instrument+Serif:ital@0;1&family=JetBrains+Mono:ital,wght@0,400;0,500;0,600;1,400&display=swap';

function serve(req, res) {
  const pathname = new URL(req.url, 'http://app:4623').pathname;
  if (pathname === '/_astro/__ui_browser/fonts.css' || pathname.startsWith('/_astro/__ui_browser/fonts/')) {
    const filename = pathname.split('/').pop();
    const isCss = pathname === '/_astro/__ui_browser/fonts.css';
    if ((req.method !== 'GET' && req.method !== 'HEAD') || (!isCss && !fonts.has(filename))) {
      res.writeHead(404).end();
      return;
    }
    const body = readFileSync(isCss ? `${root}/fonts.css` : `${root}/fonts/${filename}`);
    res.writeHead(200, {
      'content-type': isCss ? 'text/css; charset=utf-8' : 'font/woff2',
      'content-length': body.length,
      'cache-control': 'no-store',
      'x-content-type-options': 'nosniff',
    });
    res.end(req.method === 'HEAD' ? undefined : body);
    return;
  }
  const upstream = http.request({
    hostname: 'backend', port: 4623, path: req.url, method: req.method,
    headers: { ...req.headers, 'accept-encoding': 'identity',
      'x-forwarded-proto': req.socket.encrypted ? 'https' : 'http' },
  }, (source) => {
    source.on('aborted', () => res.destroy());
    res.on('close', () => { if (!res.writableFinished) source.destroy(); });
    if (req.method !== 'GET' || !source.headers['content-type']?.startsWith('text/css')) {
      source.on('error', () => res.destroy());
      res.writeHead(source.statusCode, source.headers);
      source.pipe(res);
      return;
    }
    const chunks = [];
    let size = 0;
    source.on('data', (chunk) => {
      size += chunk.length;
      if (size > 4 * 1024 * 1024) {
        source.destroy(new Error('fixture CSS exceeds 4 MiB'));
      } else {
        chunks.push(chunk);
      }
    });
    source.on('end', () => {
      const body = Buffer.from(Buffer.concat(chunks).toString('utf8').replace(
        googleFonts, (original, _quote, url) => url === dashboardFonts
          ? '@import url("/_astro/__ui_browser/fonts.css");' : original,
      ));
      const headers = { ...source.headers, 'content-length': body.length };
      delete headers.etag;
      delete headers['last-modified'];
      delete headers['transfer-encoding'];
      delete headers['content-md5'];
      res.writeHead(source.statusCode, headers).end(body);
    });
    source.on('error', () => { if (!res.headersSent) res.writeHead(502); res.end(); });
  });
  upstream.on('error', () => { if (!res.headersSent) res.writeHead(502); res.end(); });
  req.pipe(upstream);
}

// `app` is an HSTS-preloaded TLD in Chrome; a valid fixture-only certificate
// lets Chrome upgrade the test's explicit http://app URL without a TLS bypass.
https.createServer({
  key: readFileSync(`${root}/tls/app.key`),
  cert: readFileSync(`${root}/tls/app.crt`),
}, serve).listen(4623, '0.0.0.0');
