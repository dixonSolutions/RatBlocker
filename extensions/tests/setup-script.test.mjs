/**
 * The setup scripts find browsers by what is on disk, not by name.
 *
 * Each test builds a synthetic installation in a temporary home directory and
 * runs the real setup script against it with `--no-system-roots`, so nothing
 * depends on which browsers the machine running the tests happens to have.
 * Nothing below names a browser this project has heard of: the point being
 * tested is that one nobody has heard of is found anyway, and that the things
 * which merely embed an engine are not.
 *
 * The same cases run against setup.ps1 wherever PowerShell is available, so
 * the two implementations are held to one specification rather than drifting.
 */

import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { existsSync } from 'node:fs';
import { mkdtemp, mkdir, readFile, writeFile, rm, chmod } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const run = promisify(execFile);
const here = dirname(fileURLToPath(import.meta.url));
const extensions = join(here, '..');
const script = join(extensions, 'setup.sh');
const GECKO_ID = 'ratblocker@ratblocker.github.io';

let home;
let profile;

/**
 * setup.sh targets Linux and macOS; setup.ps1 targets Windows. Each is tested
 * on the platforms it is for, rather than being contorted to run everywhere.
 */
const noBash = process.platform === 'win32'
  ? 'setup.sh targets Linux and macOS; setup.ps1 covers Windows'
  : false;

/** Run setup.sh against the synthetic home, optionally feeding it answers. */
async function setup(args, { stdin = '' } = {}) {
  const pending = run('bash', [script, '--home', home, '--no-system-roots', ...args],
    { cwd: extensions, maxBuffer: 8 * 1024 * 1024 });
  pending.child.stdin.end(stdin);
  const { stdout } = await pending;
  return stdout;
}

async function inventory(args = []) {
  return JSON.parse(await setup(['--json', ...args]));
}

function browser(data, name) {
  return data.browsers.find((b) => b.name === name);
}

/** The number a browser is offered under; it depends on what else was found. */
async function numberOf(name) {
  const listing = await setup(['--list']);
  const found = new RegExp(`^\\s*([0-9]+) . ${name}`, 'm').exec(listing);
  assert.ok(found, `no numbered entry for ${name} in:\n${listing}`);
  return found[1];
}

/** A Gecko installation, for a browser that does not exist. */
async function gecko(dir, {
  name = 'Novabrowse', vendor = '', version = '9.1.0', profileKey = null,
  source = 'https://example.invalid/novabrowse', channel = null, mail = false,
} = {}) {
  await mkdir(join(dir, mail ? 'isp' : 'browser'), { recursive: true });
  await writeFile(join(dir, 'libxul.so'), 'not really a library');
  await writeFile(join(dir, mail ? 'omni.ja' : 'browser/omni.ja'), 'archive');
  await writeFile(join(dir, 'application.ini'), [
    '[App]', `Vendor=${vendor}`, `Name=${name}`, `Version=${version}`,
    ...(profileKey === null ? [] : [`Profile=${profileKey}`]),
    `SourceRepository=${source}`, '', '[Gecko]', 'MinVersion=140.0', '',
  ].join('\n'));
  if (channel !== null) {
    await mkdir(join(dir, 'defaults/pref'), { recursive: true });
    await writeFile(join(dir, 'defaults/pref/channel-prefs.js'),
      `pref("app.update.channel", "${channel}");\n`);
  }
  const binary = join(dir, name.toLowerCase());
  await writeFile(binary, '#!/bin/sh\n');
  await chmod(binary, 0o755);
}

/** A profile root, laid out the way Gecko lays one out. */
async function profileRoot(root, { name = 'abc.default', files = [], lastRanFrom = null } = {}) {
  await mkdir(join(root, name), { recursive: true });
  await writeFile(join(root, 'profiles.ini'),
    `[Profile0]\nName=default\nIsRelative=1\nPath=${name}\nDefault=1\n`);
  for (const file of files) await writeFile(join(root, name, file), '');
  if (lastRanFrom !== null) {
    await writeFile(join(root, name, 'compatibility.ini'),
      `[Compatibility]\nLastVersion=9.1.0\nLastPlatformDir=${lastRanFrom}\n`);
  }
  return join(root, name);
}

before(async () => {
  home = await mkdtemp(join(tmpdir(), 'ratblocker-setup-'));

  await gecko(join(home, '.local/lib/novabrowse'));
  profile = await profileRoot(join(home, '.novabrowse'), { files: ['places.sqlite'] });

  // A Mozilla release build, which would reject an unsigned XPI.
  await gecko(join(home, '.local/lib/strictbrowse'), {
    name: 'Strictbrowse', vendor: 'Mozilla', version: '153.0',
    source: 'https://hg.mozilla.org/releases/mozilla-release', channel: 'release',
  });
  await profileRoot(join(home, '.mozilla/strictbrowse'), { files: ['places.sqlite'] });

  // A mail client: the same engine, and not a browser.
  await gecko(join(home, '.local/lib/novamail'), { name: 'Novamail', mail: true });

  // A Chromium build, with its extensions directory compiled into the binary.
  const chromium = join(home, '.local/lib/novachrome');
  await mkdir(chromium, { recursive: true });
  for (const file of ['resources.pak', 'icudtl.dat', 'chrome_100_percent.pak']) {
    await writeFile(join(chromium, file), 'pak');
  }
  await writeFile(join(chromium, 'novachrome'), '\0/usr/share/novachrome/extensions\0');
  await chmod(join(chromium, 'novachrome'), 0o755);

  // An application that embeds Chromium but is not a browser.
  const editor = join(home, '.local/lib/novaedit');
  await mkdir(join(editor, 'resources'), { recursive: true });
  for (const file of ['resources.pak', 'icudtl.dat']) {
    await writeFile(join(editor, file), 'pak');
  }
  await writeFile(join(editor, 'resources/app.asar'), 'application code');
});

after(async () => {
  await rm(home, { recursive: true, force: true });
});

test('a browser nobody has heard of is found, with its profiles', { skip: noBash }, async () => {
  const found = browser(await inventory(), 'Novabrowse');
  assert.ok(found, 'the installation was not found');
  assert.equal(found.engine, 'gecko');
  assert.equal(found.version, '9.1.0');
  assert.equal(found.status, 'ok');
  assert.deepEqual(found.profileRoots, [join(home, '.novabrowse')]);
  assert.equal(found.binary, join(home, '.local/lib/novabrowse/novabrowse'));
});

test('an application that embeds Chromium is not offered', { skip: noBash }, async () => {
  const data = await inventory();
  assert.equal(browser(data, 'novaedit'), undefined,
    'an Electron application was offered as a browser');
  assert.ok(data.skipped >= 1, 'it should be reported as skipped, not silently dropped');
});

test('a mail client running the same engine is not offered', { skip: noBash }, async () => {
  assert.equal(browser(await inventory(), 'Novamail'), undefined);
});

test('a Chromium build is asked where it looks for extensions', { skip: noBash }, async () => {
  const found = browser(await inventory(), 'novachrome');
  assert.ok(found, 'the installation was not found');
  assert.equal(found.engine, 'chromium');
  // Read out of the binary, not out of a table.
  assert.equal(found.externalDir, '/usr/share/novachrome/extensions');
  assert.equal(found.status, 'needs-root');
});

test('a build that enforces signing is refused, not half-installed', { skip: noBash }, async () => {
  const found = browser(await inventory(), 'Strictbrowse');
  assert.equal(found.signing, 'yes');
  assert.equal(found.status, 'refused');
});

test('a selection typed at the prompt is what gets acted on', { skip: noBash }, async () => {
  const number = await numberOf('Novabrowse');
  const out = await setup(['--dry-run'], { stdin: `${number}\n` });
  assert.match(out, /would install .*abc\.default/);
  assert.doesNotMatch(out, /Strictbrowse\n\s+would/);
});

test('quitting at the prompt does nothing at all', { skip: noBash }, async () => {
  const out = await setup(['--dry-run'], { stdin: 'q\n' });
  assert.match(out, /Nothing selected/);
  assert.doesNotMatch(out, /would install/);
});

test('choosing everything skips what cannot work', { skip: noBash }, async () => {
  const out = await setup(['--all', '--yes', '--dry-run']);
  const acted = out.slice(out.indexOf('Would do'));
  assert.match(acted, /would install/);
  assert.doesNotMatch(acted, /Strictbrowse/);
});

test('installing, then uninstalling, leaves the profile as it was', { skip: noBash }, async () => {
  const target = join(profile, 'extensions', `${GECKO_ID}.xpi`);
  const number = await numberOf('Novabrowse');

  const installed = await setup(['--select', number, '--yes']);
  assert.match(installed, /installed/);
  assert.ok(existsSync(target), 'the XPI was not written into the profile');

  // An unsigned build needs the preference, fenced off by a marker.
  const prefs = await readFile(join(profile, 'user.js'), 'utf8');
  assert.match(prefs, /xpinstall\.signatures\.required/);

  const removed = await setup(['--uninstall', '--select', number, '--yes']);
  assert.match(removed, /removed/);
  assert.equal(existsSync(target), false, 'the XPI was left behind');
  assert.equal(existsSync(join(profile, 'user.js')), false, 'user.js was left behind');
});

test('update mode installs forward and never backward', { skip: noBash }, async () => {
  const number = await numberOf('Novabrowse');
  const db = join(profile, 'extensions.json');

  await writeFile(db, JSON.stringify({ addons: [{ id: GECKO_ID, version: '99.0.0' }] }));
  assert.match(await setup(['--update', '--select', number, '--yes', '--dry-run']),
    /up to date \(99\.0\.0\)/);

  await writeFile(db, JSON.stringify({ addons: [{ id: GECKO_ID, version: '0.0.9' }] }));
  assert.match(await setup(['--update', '--select', number, '--yes', '--dry-run']),
    /0\.0\.9 -> /);

  // "0.10.0" sorts before "0.9.0" as text and after it as a version.
  await writeFile(db, JSON.stringify({ addons: [{ id: GECKO_ID, version: '0.10.0' }] }));
  assert.match(await setup(['--update', '--select', number, '--yes', '--dry-run']),
    /up to date \(0\.10\.0\)/);

  await rm(db, { force: true });
});

test('a profile that has moved is matched to its installation', { skip: noBash }, async () => {
  const moved = await mkdtemp(join(tmpdir(), 'ratblocker-moved-'));
  try {
    const install = join(moved, '.local/lib/wanderer');
    await gecko(install, { name: 'Wanderer', profileKey: 'wanderer' });
    // The build says ~/.wanderer; this profile is somewhere else entirely, and
    // compatibility.ini is what settles it.
    await profileRoot(join(moved, '.config/wanderer'),
      { files: ['places.sqlite'], lastRanFrom: install });

    const data = JSON.parse(await run('bash',
      [script, '--home', moved, '--no-system-roots', '--json'],
      { cwd: extensions }).then((r) => r.stdout));
    const found = data.browsers.find((b) => b.name === 'Wanderer');
    assert.ok(found, 'the installation was not found');
    assert.deepEqual(found.profileRoots, [join(moved, '.config/wanderer')]);
    assert.equal(data.browsers.length, 1,
      'the profile should attach to its install, not appear as a second browser');
  } finally {
    await rm(moved, { recursive: true, force: true });
  }
});

test('a mail profile whose client is gone is not offered either', { skip: noBash }, async () => {
  const orphan = await mkdtemp(join(tmpdir(), 'ratblocker-orphan-'));
  try {
    await profileRoot(join(orphan, '.novamail'),
      { files: ['places.sqlite', 'ImapMail', 'abook.sqlite'] });
    const data = JSON.parse(await run('bash',
      [script, '--home', orphan, '--no-system-roots', '--json'],
      { cwd: extensions }).then((r) => r.stdout));
    assert.equal(data.browsers.length, 0,
      'a mail profile was offered as a browser');
  } finally {
    await rm(orphan, { recursive: true, force: true });
  }
});

/* ------------------------------------------------------------- PowerShell */

/**
 * The same specification, against the Windows implementation.
 *
 * These skip where PowerShell is not installed and run in CI, which has it on
 * both the Linux and the Windows runners — so setup.ps1 is held to the same
 * behaviour as setup.sh rather than being taken on trust.
 */
const pwsh = await run('pwsh', ['-NoProfile', '-Command', '$PSVersionTable.PSVersion.Major'])
  .then((r) => r.stdout.trim())
  .catch(() => null);
const noPwsh = pwsh === null ? 'PowerShell is not installed here; CI runs these' : false;

let winHome;

/** A Windows-shaped synthetic machine: installs under Program Files, profiles under AppData. */
async function windowsHome() {
  const root = await mkdtemp(join(tmpdir(), 'ratblocker-win-'));
  const programFiles = join(root, 'Program Files');

  const install = join(programFiles, 'Novabrowse');
  await gecko(install);
  await writeFile(join(install, 'xul.dll'), 'engine');
  await writeFile(join(install, 'novabrowse.exe'), 'launcher');
  await profileRoot(join(root, 'AppData/Roaming/Novabrowse'), { files: ['places.sqlite'] });

  const strict = join(programFiles, 'Strictbrowse');
  await gecko(strict, {
    name: 'Strictbrowse', vendor: 'Mozilla',
    source: 'https://hg.mozilla.org/releases/mozilla-release', channel: 'release',
  });
  await writeFile(join(strict, 'xul.dll'), 'engine');
  await profileRoot(join(root, 'AppData/Roaming/Mozilla/Strictbrowse'), { files: ['places.sqlite'] });

  const mail = join(programFiles, 'Novamail');
  await gecko(mail, { name: 'Novamail', mail: true });
  await writeFile(join(mail, 'xul.dll'), 'engine');

  const editor = join(programFiles, 'Novaedit');
  await mkdir(join(editor, 'resources'), { recursive: true });
  for (const file of ['resources.pak', 'icudtl.dat']) await writeFile(join(editor, file), 'pak');
  await writeFile(join(editor, 'resources/app.asar'), 'application code');

  return root;
}

async function powershell(args, { stdin = '' } = {}) {
  const pending = run('pwsh', ['-NoProfile', '-File', join(extensions, 'setup.ps1'),
    '-Home_', winHome, '-NoSystemRoots', ...args], { cwd: extensions, maxBuffer: 8 * 1024 * 1024 });
  pending.child.stdin.end(stdin);
  const { stdout } = await pending;
  return stdout;
}

test('setup.ps1 finds a browser by its engine, and excludes what is not one',
  { skip: noPwsh }, async () => {
    winHome ??= await windowsHome();
    const data = JSON.parse(await powershell(['-Json']));
    const names = data.browsers.map((b) => b.name);
    assert.ok(names.includes('Novabrowse'), `expected Novabrowse in ${names.join(', ')}`);
    assert.ok(!names.includes('Novaedit'), 'an Electron application was offered as a browser');
    assert.ok(!names.includes('Novamail'), 'a mail client was offered as a browser');
  });

test('setup.ps1 refuses a build that enforces signing', { skip: noPwsh }, async () => {
  winHome ??= await windowsHome();
  const data = JSON.parse(await powershell(['-Json']));
  const strict = data.browsers.find((b) => b.name === 'Strictbrowse');
  assert.ok(strict, 'the Mozilla build was not found');
  assert.equal(strict.signing, 'yes');
  assert.equal(strict.status, 'refused');
});

test('setup.ps1 installs and uninstalls the same way setup.sh does',
  { skip: noPwsh }, async () => {
    winHome ??= await windowsHome();
    const data = JSON.parse(await powershell(['-Json']));
    const index = data.browsers.findIndex((b) => b.name === 'Novabrowse') + 1;
    const target = join(winHome, 'AppData/Roaming/Novabrowse/abc.default/extensions',
      `${GECKO_ID}.xpi`);

    assert.match(await powershell(['-Select', String(index), '-Yes']), /installed/);
    assert.ok(existsSync(target), 'the XPI was not written into the profile');

    assert.match(await powershell(['-Uninstall', '-Select', String(index), '-Yes']), /removed/);
    assert.equal(existsSync(target), false, 'the XPI was left behind');
  });

after(async () => {
  if (winHome) await rm(winHome, { recursive: true, force: true });
});
