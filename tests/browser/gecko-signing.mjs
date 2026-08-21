/**
 * Does this Gecko browser enforce Mozilla's extension signature check?
 *
 * `xpinstall.signatures.required=false` is only honoured by builds compiled
 * with MOZ_REQUIRE_SIGNING off — Developer Edition, Nightly, ESR, Mozilla's
 * unbranded builds, and forks that chose to disable it. There is no way to ask
 * a binary directly, so this asks the only question that matters: does a
 * permanent install of an unsigned XPI actually succeed?
 *
 * Usage: node gecko-signing.mjs <path-to-browser-binary> <path-to-unsigned.xpi>
 */

import { spawn } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

import { connect } from './marionette.mjs';

const PORT = 2830;

async function main() {
  const binary = resolve(process.argv[2] ?? 'firefox');
  const xpi = resolve(process.argv[3] ?? 'dist/ratblocker-firefox-0.1.0.xpi');
  const profile = await mkdtemp(join(tmpdir(), 'gecko-signing-'));
  const logs = [];
  let browser;
  let client;

  try {
    await writeFile(
      join(profile, 'user.js'),
      [
        'user_pref("xpinstall.signatures.required", false);',
        'user_pref("extensions.langpacks.signatures.required", false);',
        'user_pref("browser.shell.checkDefaultBrowser", false);',
        'user_pref("datareporting.policy.dataSubmissionEnabled", false);',
        'user_pref("app.update.enabled", false);',
        `user_pref("marionette.port", ${PORT});`,
        '',
      ].join('\n'),
    );

    browser = spawn(binary, ['--marionette', '--headless', '--no-remote', '--profile', profile], {
      stdio: ['ignore', 'pipe', 'pipe'],
      env: { ...process.env, MOZ_DISABLE_CONTENT_SANDBOX: '1' },
    });
    browser.stderr.on('data', (b) => logs.push(String(b).trim()));
    browser.stdout.on('data', (b) => logs.push(String(b).trim()));

    client = await connect(PORT);
    const session = await client.send('WebDriver:NewSession', {});
    const { browserName, browserVersion } = session.capabilities;
    console.log(`browser:  ${browserName} ${browserVersion}`);
    console.log(`binary:   ${binary}`);

    // `temporary: false` is the whole point: a temporary install bypasses the
    // signature check, so it would prove nothing.
    try {
      const result = await client.send('Addon:Install', { path: xpi, temporary: false });
      console.log(`\nPERMANENT UNSIGNED INSTALL SUCCEEDED: ${result.value ?? JSON.stringify(result)}`);
      console.log('=> This build does NOT enforce signing. Self-hosted, unsigned');
      console.log('   distribution works here with no Mozilla involvement.');
      process.exitCode = 0;
    } catch (error) {
      console.log(`\nPermanent unsigned install refused: ${error.message}`);
      console.log('=> This build DOES enforce signing. It needs an XPI signed by');
      console.log('   Mozilla (AMO unlisted signing produces one).');
      process.exitCode = 1;
    }
  } catch (error) {
    console.error('harness error:', error.message);
    process.exitCode = 2;
  } finally {
    try {
      await client?.send('Marionette:Quit', {});
    } catch {
      browser?.kill('SIGTERM');
    }
    await new Promise((r) => setTimeout(r, 1200));
    await rm(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 400 });
    for (const line of logs.slice(-6)) console.log(`  [log] ${line}`);
  }
}

await main();
