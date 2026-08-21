/**
 * A hermetic origin for the browser integration tests.
 *
 * Every probe hostname is resolved to this local server — Chromium via
 * `--host-resolver-rules`, Firefox via the `network.dns.localDomains` pref —
 * so the test needs no internet and no real DNS. That turns an ambiguous
 * signal into a clean one: an unblocked request returns HTTP 200 and the
 * script fires `load`, while a blocked one fires `error`. There is no third
 * outcome to squint at.
 *
 * The page is served from `probe.localhost` rather than `127.0.0.1` because
 * EasyList ships `@@://127.0.0.1$generichide`, which switches generic element
 * hiding off on loopback addresses.
 *
 * It is served over HTTPS with a throwaway self-signed certificate, which both
 * browsers are told to accept. Plain HTTP would be unusable here: most probe
 * hostnames are in the HSTS preload list, so the browser would upgrade the
 * request itself and the resulting failure would be indistinguishable from a
 * block. Over HTTPS the server always answers, so a failure can only mean the
 * extension stopped it.
 */

/** Generate a short-lived self-signed certificate for the fixture. */
function selfSignedCertificate() {
  const dir = mkdtempSync(join(tmpdir(), 'ratblocker-cert-'));
  const key = join(dir, 'key.pem');
  const cert = join(dir, 'cert.pem');
  execFileSync('openssl', [
    'req', '-x509', '-newkey', 'rsa:2048', '-nodes',
    '-keyout', key, '-out', cert, '-days', '1',
    '-subj', '/CN=probe.localhost',
    '-addext', 'subjectAltName=DNS:probe.localhost,DNS:*.localhost,IP:127.0.0.1',
  ], { stdio: 'ignore' });
  return { key: readFileSync(key), cert: readFileSync(cert), dir };
}

import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createServer } from 'node:https';

/** `expect: 'blocked'` means some filter rule must stop this request. */
export const PROBES = [
  { host: 'www.google-analytics.com', path: '/analytics.js', expect: 'blocked' },
  { host: 'securepubads.g.doubleclick.net', path: '/tag/js/gpt.js', expect: 'blocked' },
  { host: 'connect.facebook.net', path: '/en_US/fbevents.js', expect: 'blocked' },
  { host: 'static.doubleclick.net', path: '/instream/ad_status.js', expect: 'blocked' },
  { host: 'www.googletagservices.com', path: '/tag/js/gpt.js', expect: 'blocked' },
  { host: 'cdn.taboola.com', path: '/libtrc/x.js', expect: 'blocked' },
  { host: 'sb.scorecardresearch.com', path: '/beacon.js', expect: 'blocked' },
  { host: 'pagead2.googlesyndication.com', path: '/pagead/js/adsbygoogle.js', expect: 'blocked' },
  // Controls: nothing in EasyList or EasyPrivacy should stop these.
  { host: 'cdn.jsdelivr.net', path: '/npm/lib.js', expect: 'allowed' },
  { host: 'fonts.googleapis.com', path: '/css2.js', expect: 'allowed' },
  { host: 'assets.example.org', path: '/app.js', expect: 'allowed' },
];

export function probeHosts() {
  return [...new Set(PROBES.map((p) => p.host))];
}

function page(port) {
  const probes = PROBES.map((p) => ({
    url: `https://${p.host}:${port}${p.path}`,
    expect: p.expect,
  }));
  return `<!doctype html>
<meta charset="utf-8">
<title>RatBlocker probe</title>
<h1>RatBlocker probe page</h1>
<!-- '###AdBar' is a generic element-hiding rule in EasyList. -->
<div id="AdBar">generic ad slot</div>
<div id="not-an-ad">ordinary content</div>
<script>
window.__probes = ${JSON.stringify(probes)};
window.__results = {};
window.__done = false;
(function () {
  let outstanding = window.__probes.length;
  const finish = () => { if (--outstanding === 0) window.__done = true; };
  for (const probe of window.__probes) {
    const s = document.createElement('script');
    s.src = probe.url;
    s.addEventListener('load', () => { window.__results[probe.url] = 'loaded'; finish(); });
    s.addEventListener('error', () => { window.__results[probe.url] = 'failed'; finish(); });
    document.head.append(s);
  }
  // Never hang the harness if a request is silently swallowed.
  setTimeout(() => { window.__done = true; }, 8000);
})();
</script>
`;
}

export function startFixture() {
  const tls = selfSignedCertificate();
  return new Promise((resolve) => {
    const server = createServer({ key: tls.key, cert: tls.cert }, (req, res) => {
      const url = new URL(req.url, 'http://placeholder');
      if (url.pathname === '/probe') {
        res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
        res.end(page(server.address().port));
        return;
      }
      // Any other path is a probe target: answer with a trivially valid script.
      res.writeHead(200, {
        'content-type': 'application/javascript',
        'access-control-allow-origin': '*',
      });
      res.end('/* probe target reached */\n');
    });
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      resolve({
        port,
        url: `https://probe.localhost:${port}/probe`,
        close: () => new Promise((r) => server.close(r)),
      });
    });
  });
}

/** Turn the page's raw results into pass/fail lines. */
export function evaluateResults(results, port) {
  const lines = [];
  let failures = 0;
  for (const probe of PROBES) {
    const url = `https://${probe.host}:${port}${probe.path}`;
    const outcome = results[url];
    const blocked = outcome === 'failed';
    const ok = probe.expect === 'blocked' ? blocked : outcome === 'loaded';
    if (!ok) failures += 1;
    lines.push(
      `  ${ok ? 'PASS' : 'FAIL'}  ${probe.expect.padEnd(7)} ${probe.host}${probe.path}` +
        `  (${outcome ?? 'no result'})`,
    );
  }
  return { lines, failures };
}
