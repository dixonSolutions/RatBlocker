/**
 * Serve `dist/` so browsers have somewhere to fetch updates from.
 *
 * For when the downloads cannot be published anywhere reachable — an isolated
 * network, or a machine that should not depend on an external host. Chrome
 * accepts a plain-HTTP update manifest, including on localhost, so this is
 * enough to keep Chromium and Chrome current on every platform, and it is the
 * only route on Windows and macOS where an off-store install must go through
 * enterprise policy.
 *
 * Firefox is different: Gecko requires HTTPS for update_url. That is not a
 * problem in practice, because a Gecko install is updated by replacing the
 * file in the profile (`install.mjs --update`) and never needs a server.
 *
 * Usage:
 *   node serve.mjs [--port 8080] [--host 127.0.0.1]
 *
 * Package with a matching base first, so the URLs inside the artefacts agree
 * with where they are actually served from:
 *   RATBLOCKER_UPDATE_BASE=http://127.0.0.1:8080 node package.mjs
 */

import { createServer } from 'node:http';
import { createReadStream } from 'node:fs';
import { stat } from 'node:fs/promises';
import { extname, join, normalize, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '..', 'dist');

const args = process.argv.slice(2);
const flag = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 && args[i + 1] !== undefined ? args[i + 1] : fallback;
};
const port = Number.parseInt(flag('port', '8080'), 10);
// Loopback by default: this serves installable extension packages, which is
// not something to expose to a network without deciding to.
const host = flag('host', '127.0.0.1');

const TYPES = {
  '.crx': 'application/x-chrome-extension',
  '.xpi': 'application/x-xpinstall',
  '.xml': 'application/xml',
  '.json': 'application/json',
  '.txt': 'text/plain; charset=utf-8',
  '.reg': 'text/plain; charset=utf-8',
  '.plist': 'application/xml',
};

const server = createServer(async (request, response) => {
  const url = new URL(request.url ?? '/', `http://${request.headers.host}`);
  // Resolve inside the root and confirm it stayed there, so a crafted path
  // cannot read outside dist/.
  const target = resolve(join(root, normalize(decodeURIComponent(url.pathname))));
  if (target !== root && !target.startsWith(`${root}/`)) {
    response.writeHead(403).end('forbidden');
    return;
  }
  try {
    const info = await stat(target);
    if (info.isDirectory()) {
      response.writeHead(404).end('not found');
      return;
    }
    response.writeHead(200, {
      'content-type': TYPES[extname(target)] ?? 'application/octet-stream',
      'content-length': info.size,
      'cache-control': 'no-cache',
    });
    createReadStream(target).pipe(response);
    console.log(`200 ${url.pathname}`);
  } catch {
    response.writeHead(404).end('not found');
    console.log(`404 ${url.pathname}`);
  }
});

server.listen(port, host, () => {
  console.log(`serving ${root}`);
  console.log(`  http://${host}:${port}/`);
  console.log('');
  console.log('Package with a matching base so the artefacts agree:');
  console.log(`  RATBLOCKER_UPDATE_BASE=http://${host}:${port} node package.mjs`);
});
