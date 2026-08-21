/**
 * Integration test: load the built Chromium extension into the user's real
 * Chromium and assert that it blocks what it should and leaves alone what it
 * should not.
 *
 * A throwaway profile directory is used so the test never touches the user's
 * own Chromium profile.
 */

import { spawn } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

import { connect, waitForEndpoint } from './cdp.mjs';
import { evaluateResults, startFixture } from './fixture.mjs';

const EXTENSION = resolve(import.meta.dirname, '../../extensions/chromium/build');
const PORT = 9333;

async function main() {
  const profile = await mkdtemp(join(tmpdir(), 'ratblocker-chromium-'));
  const fixture = await startFixture();
  const logs = [];
  let chromium;

  try {
    chromium = spawn(
      'chromium',
      [
        `--user-data-dir=${profile}`,
        `--remote-debugging-port=${PORT}`,
        `--load-extension=${EXTENSION}`,
        `--disable-extensions-except=${EXTENSION}`,
        '--headless=new',
        '--ignore-certificate-errors',
        // Resolve every hostname to the fixture, so the test needs no network.
        `--host-resolver-rules=MAP * 127.0.0.1`,
        '--no-first-run',
        '--no-default-browser-check',
        '--disable-background-networking',
        '--disable-component-update',
        '--disable-sync',
        'about:blank',
      ],
      { stdio: ['ignore', 'pipe', 'pipe'] },
    );
    chromium.stderr.on('data', (b) => logs.push(`[chromium] ${b}`.trim()));

    const version = await waitForEndpoint(`http://127.0.0.1:${PORT}/json/version`);
    console.log(`browser: ${version.Browser}`);

    const client = await connect(version.webSocketDebuggerUrl);
    await client.send('Target.setDiscoverTargets', { discover: true });

    // 1. Find the extension's service worker and read what it logged.
    const workerErrors = [];
    let workerReady = null;
    const deadline = Date.now() + 20000;
    while (Date.now() < deadline && workerReady === null) {
      const { targetInfos } = await client.send('Target.getTargets');
      const worker = targetInfos.find(
        (t) => t.type === 'service_worker' && t.url.startsWith('chrome-extension://'),
      );
      if (worker !== undefined) {
        const { sessionId } = await client.send('Target.attachToTarget', {
          targetId: worker.targetId,
          flatten: true,
        });
        client.on((message) => {
          if (message.sessionId !== sessionId) return;
          if (message.method === 'Runtime.consoleAPICalled') {
            const text = message.params.args
              .map((a) => a.value ?? a.description ?? '')
              .join(' ');
            logs.push(`[worker:${message.params.type}] ${text}`);
            if (message.params.type === 'error') workerErrors.push(text);
            if (text.includes('core') && text.includes('ready')) workerReady = text;
          }
          if (message.method === 'Runtime.exceptionThrown') {
            const d = message.params.exceptionDetails;
            const text = d.exception?.description ?? d.text;
            logs.push(`[worker:exception] ${text}`);
            workerErrors.push(text);
          }
        });
        await client.send('Runtime.enable', {}, sessionId);
        // The worker may already have logged before we attached; ask it.
        const probe = await client.send(
          'Runtime.evaluate',
          {
            expression:
              'JSON.stringify({rulesets: chrome.runtime.getManifest().declarative_net_request.rule_resources.length})',
            returnByValue: true,
          },
          sessionId,
        );
        logs.push(`[worker] manifest ${probe.result.value}`);
        workerReady ??= 'attached';
      }
      if (workerReady === null) await new Promise((r) => setTimeout(r, 300));
    }
    if (workerReady === null) throw new Error('extension service worker never appeared');

    // Give the worker a moment to finish loading the WASM core.
    await new Promise((r) => setTimeout(r, 3000));

    // 2. Open the probe page and watch how each request fails.
    const { targetId } = await client.send('Target.createTarget', { url: 'about:blank' });
    const { sessionId: page } = await client.send('Target.attachToTarget', {
      targetId,
      flatten: true,
    });

    await client.send('Page.enable', {}, page);
    await client.send('Page.navigate', { url: fixture.url }, page);

    // The page reports each probe's outcome itself. Over HTTPS with a served
    // certificate the server always answers, so `failed` can only mean the
    // extension stopped the request.
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

    // 3. Report.
    const { lines, failures: probeFailures } = evaluateResults(results, fixture.port);
    console.log('\nprobe results');
    for (const line of lines) console.log(line);
    let failures = probeFailures;

    // 4. Cosmetic filtering: a generic `##.ad` rule should hide the slot.
    const hidden = await client.send(
      'Runtime.evaluate',
      {
        expression: `(() => {
          const el = document.getElementById('AdBar');
          if (el === null) return 'missing';
          return getComputedStyle(el).display;
        })()`,
        returnByValue: true,
      },
      page,
    );
    const control = await client.send(
      'Runtime.evaluate',
      {
        expression: "getComputedStyle(document.getElementById('not-an-ad')).display",
        returnByValue: true,
      },
      page,
    );
    const cosmeticOk = hidden.result.value === 'none' && control.result.value !== 'none';
    console.log(
      `\ncosmetic: #AdBar display = ${hidden.result.value}, control #not-an-ad = ${control.result.value} ` +
        `${cosmeticOk ? '(PASS)' : '(FAIL)'}`,
    );
    if (!cosmeticOk) failures += 1;

    if (workerErrors.length > 0) {
      console.log('\nservice worker errors:');
      for (const error of workerErrors) console.log(`  ${error}`);
      failures += workerErrors.length;
    }

    console.log(`\n${failures === 0 ? 'ALL CHECKS PASSED' : `${failures} CHECK(S) FAILED`}`);
    client.close();
    process.exitCode = failures === 0 ? 0 : 1;
  } catch (error) {
    console.error('harness error:', error.message);
    process.exitCode = 2;
  } finally {
    console.log('\n--- logs ---');
    for (const line of logs.slice(-40)) console.log(line);
    chromium?.kill('SIGTERM');
    await fixture.close();
    // Chromium flushes its profile on exit; give it a moment before removing.
    await new Promise((r) => setTimeout(r, 1500));
    await rm(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 500 });
  }
}

await main();
