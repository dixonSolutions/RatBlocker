/**
 * Integration test: install the built Firefox extension as a temporary add-on
 * in the user's real Firefox and assert what it blocks and hides.
 *
 * Runs against a throwaway profile so it never touches the user's own.
 */

import { spawn } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

import { evaluateResults, probeHosts, startFixture } from './fixture.mjs';
import { connect } from './marionette.mjs';

// Any Gecko browser can be driven here: Firefox, Zen, LibreWolf, Floorp.
// The extension is identical; only the signing policy differs between builds.
const BROWSER = process.env.GECKO_BINARY ?? 'firefox';
const EXTENSION = resolve(import.meta.dirname, '../../extensions/firefox/build');
const MARIONETTE_PORT = 2828;

async function main() {
  const profile = await mkdtemp(join(tmpdir(), 'ratblocker-firefox-'));
  const fixture = await startFixture();
  const logs = [];
  let firefox;
  let client;

  try {
    // Resolve every probe hostname to loopback so the test needs no network.
    await writeFile(
      join(profile, 'user.js'),
      [
        `user_pref("network.dns.localDomains", "${[...probeHosts(), 'probe.localhost'].join(',')}");`,
        'user_pref("browser.shell.checkDefaultBrowser", false);',
        'user_pref("datareporting.policy.dataSubmissionEnabled", false);',
        'user_pref("datareporting.healthreport.uploadEnabled", false);',
        'user_pref("toolkit.telemetry.enabled", false);',
        'user_pref("app.update.enabled", false);',
        'user_pref("browser.aboutwelcome.enabled", false);',
        'user_pref("extensions.autoDisableScopes", 0);',
        // Temporary add-ons are unsigned by definition.
        'user_pref("xpinstall.signatures.required", false);',
        `user_pref("marionette.port", ${MARIONETTE_PORT});`,
        '',
      ].join('\n'),
    );

    firefox = spawn(
      BROWSER,
      ['--marionette', '--headless', '--no-remote', '--profile', profile, 'about:blank'],
      { stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, MOZ_DISABLE_CONTENT_SANDBOX: '1' } },
    );
    firefox.stderr.on('data', (b) => logs.push(`[firefox] ${String(b).trim()}`));
    firefox.stdout.on('data', (b) => logs.push(`[firefox] ${String(b).trim()}`));

    client = await connect(MARIONETTE_PORT);
    const session = await client.send('WebDriver:NewSession', {
      // The fixture uses a throwaway self-signed certificate.
      acceptInsecureCerts: true,
    });
    console.log(
      `browser: ${session.capabilities.browserName} ${session.capabilities.browserVersion} (${BROWSER})`,
    );

    // 1. Install the extension as a temporary add-on.
    const addon = await client.send('Addon:Install', { path: EXTENSION, temporary: true });
    console.log(`installed add-on: ${addon.value ?? JSON.stringify(addon)}`);

    // The background page has to fetch and index a 12 MiB database first.
    await new Promise((r) => setTimeout(r, 6000));

    // 2. Load the probe page and let it report.
    await client.send('WebDriver:Navigate', { url: fixture.url });
    let results = {};
    for (let attempt = 0; attempt < 30; attempt += 1) {
      const done = await client.send('WebDriver:ExecuteScript', {
        script: 'return JSON.stringify({done: window.__done === true, results: window.__results});',
        args: [],
      });
      const state = JSON.parse(done.value);
      results = state.results ?? {};
      if (state.done) break;
      await new Promise((r) => setTimeout(r, 500));
    }

    const { lines, failures: probeFailures } = evaluateResults(results, fixture.port);
    console.log('\nprobe results');
    for (const line of lines) console.log(line);
    let failures = probeFailures;

    // 3. Cosmetic filtering.
    const cosmetic = await client.send('WebDriver:ExecuteScript', {
      script: `return JSON.stringify({
        ad: getComputedStyle(document.getElementById('AdBar')).display,
        control: getComputedStyle(document.getElementById('not-an-ad')).display
      });`,
      args: [],
    });
    const { ad, control } = JSON.parse(cosmetic.value);
    const cosmeticOk = ad === 'none' && control !== 'none';
    console.log(
      `\ncosmetic: #AdBar display = ${ad}, control #not-an-ad = ${control} ` +
        `${cosmeticOk ? '(PASS)' : '(FAIL)'}`,
    );
    if (!cosmeticOk) failures += 1;

    console.log(`\n${failures === 0 ? 'ALL CHECKS PASSED' : `${failures} CHECK(S) FAILED`}`);
    process.exitCode = failures === 0 ? 0 : 1;
  } catch (error) {
    console.error('harness error:', error.message);
    process.exitCode = 2;
  } finally {
    console.log('\n--- logs ---');
    for (const line of logs.slice(-30)) console.log(line);
    try {
      await client?.send('Marionette:Quit', {});
    } catch {
      firefox?.kill('SIGTERM');
    }
    await fixture.close();
    await new Promise((r) => setTimeout(r, 1500));
    await rm(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 500 });
  }
}

await main();
