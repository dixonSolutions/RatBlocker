/**
 * Update mode decides correctly from the browser's own extension database.
 *
 * Runs the real install.mjs against a synthetic HOME, so profile discovery,
 * version comparison and the dry-run plan are all exercised without touching a
 * real browser profile.
 */

import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, rm, chmod } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const run = promisify(execFile);
const here = dirname(fileURLToPath(import.meta.url));
const installer = join(here, '..', 'install.mjs');

let home;
let profile;

before(async () => {
  home = await mkdtemp(join(tmpdir(), 'ratblocker-home-'));
  profile = join(home, '.zen/abc.default');
  await mkdir(join(profile, 'extensions'), { recursive: true });
  await mkdir(join(home, '.tarball-installations/zen'), { recursive: true });
  const binary = join(home, '.tarball-installations/zen/zen');
  await writeFile(binary, '#!/bin/sh\n');
  await chmod(binary, 0o755);
  await writeFile(
    join(home, '.zen/profiles.ini'),
    '[Profile0]\nName=default\nIsRelative=1\nPath=abc.default\nDefault=1\n',
  );
});

after(async () => {
  await rm(home, { recursive: true, force: true });
});

/** Claim `version` is installed, then ask what update mode would do. */
async function planWith(version) {
  const db = version === null
    ? { addons: [] }
    : { addons: [{ id: 'ratblocker@ratblocker.github.io', version, active: true }] };
  await writeFile(join(profile, 'extensions.json'), JSON.stringify(db));
  const { stdout } = await run(
    process.execPath,
    [installer, '--update', '--dry-run', 'zen'],
    { env: { ...process.env, HOME: home }, cwd: join(here, '..') },
  );
  return stdout;
}

test('skips a profile already at the packaged version', async () => {
  const out = await planWith('0.1.0');
  assert.match(out, /up to date \(0\.1\.0\)/);
  assert.doesNotMatch(out, /-> 0\.1\.0/);
});

test('updates a profile holding an older version', async () => {
  const out = await planWith('0.0.9');
  assert.match(out, /0\.0\.9 -> 0\.1\.0/);
});

test('does not downgrade a profile holding a newer version', async () => {
  const out = await planWith('0.2.0');
  assert.match(out, /up to date \(0\.2\.0\)/);
  assert.doesNotMatch(out, /0\.2\.0 -> /);
});

test('installs where nothing is present yet', async () => {
  const out = await planWith(null);
  assert.match(out, /nothing -> 0\.1\.0/);
});

test('compares numerically, not lexically', async () => {
  // "0.10.0" sorts before "0.9.0" as text, and after it as a version.
  const out = await planWith('0.10.0');
  assert.match(out, /up to date \(0\.10\.0\)/);
});
