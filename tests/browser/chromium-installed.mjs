/**
 * Verify the CRX installs itself through Chromium's external-extension
 * mechanism — no Web Store, no `--load-extension` flag — and that the
 * installed copy actually filters.
 *
 * A throwaway profile is used, so this proves the install happens for a fresh
 * profile the way it would for a real user, without touching their own.
 */

import { spawn } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { connect, waitForEndpoint } from './cdp.mjs';
import { evaluateResults, startFixture } from './fixture.mjs';

const PORT = 9334;
const EXPECTED_ID = 'mkkpcbjiinhopbipddkpjjaeffjmfnnb';

async function main() {
  const profile = await mkdtemp(join(tmpdir(), 'ratblocker-installed-'));
  const fixture = await startFixture();
  const logs = [];
  let chromium;

  try {
    chromium = spawn(
      'chromium',
      [
        `--user-data-dir=${profile}`,
        `--remote-debugging-port=${PORT}`,
        '--headless=new',
        '--ignore-certificate-errors',
        '--host-resolver-rules=MAP * 127.0.0.1',
        '--no-first-run',
        '--no-default-browser-check',
        '--disable-background-networking',
        '--disable-component-update',
        'about:blank',
      ],
      { stdio: ['ignore', 'pipe', 'pipe'] },
    );
    chromium.stderr.on('data', (b) => logs.push(`[chromium] ${String(b).trim()}`));

    const version = await waitForEndpoint(`http://127.0.0.1:${PORT}/json/version`);
    console.log(`browser: ${version.Browser}`);
    const client = await connect(version.webSocketDebuggerUrl);

    // Wait for the externally-installed extension's worker to appear.
    let found = false;
    const deadline = Date.now() + 45000;
    while (Date.now() < deadline && !found) {
      const { targetInfos } = await client.send('Target.getTargets');
      const worker = targetInfos.find(
        (t) => t.type === 'service_worker' && t.url.includes(EXPECTED_ID),
      );
      if (worker !== undefined) {
        found = true;
        console.log(`extension installed from the CRX: ${EXPECTED_ID}`);
        break;
      }
      await new Promise((r) => setTimeout(r, 500));
    }
    if (!found) {
      const { targetInfos } = await client.send('Target.getTargets');
      console.log('targets seen:');
      for (const t of targetInfos) console.log(`  ${t.type} ${t.url}`);
      throw new Error('the externally-installed extension never started');
    }

    await new Promise((r) => setTimeout(r, 3000));

    const { targetId } = await client.send('Target.createTarget', { url: 'about:blank' });
    const { sessionId: page } = await client.send('Target.attachToTarget', {
      targetId,
      flatten: true,
    });
    await client.send('Page.enable', {}, page);
    await client.send('Page.navigate', { url: fixture.url }, page);

    let results = {};
    for (let attempt = 0; attempt < 40; attempt += 1) {
      const state = await client.send(
        'Runtime.evaluate',
        {
          expression:
            'JSON.stringify({done: window.__done === true, results: window.__results || {}})',
          returnByValue: true,
        },
        page,
      );
      const parsed = JSON.parse(state.result.value ?? '{}');
      results = parsed.results ?? {};
      if (parsed.done === true) break;
      await new Promise((r) => setTimeout(r, 500));
    }

    const { lines, failures } = evaluateResults(results, fixture.port);
    console.log('\nprobe results');
    for (const line of lines) console.log(line);
    console.log(`\n${failures === 0 ? 'ALL CHECKS PASSED' : `${failures} CHECK(S) FAILED`}`);
    client.close();
    process.exitCode = failures === 0 ? 0 : 1;
  } catch (error) {
    console.error('harness error:', error.message);
    process.exitCode = 2;
  } finally {
    chromium?.kill('SIGTERM');
    await fixture.close();
    await new Promise((r) => setTimeout(r, 1500));
    await rm(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 500 });
    for (const line of logs.slice(-8)) console.log(line);
  }
}

await main();
