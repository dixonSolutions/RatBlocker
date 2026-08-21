/**
 * Install the built extensions into the browsers on this machine.
 *
 * Usage:
 *   node install.mjs                 install into every browser found
 *   node install.mjs --dry-run       show what would happen, change nothing
 *   node install.mjs --uninstall     remove it again
 *   node install.mjs zen firefox     restrict to named browsers
 *
 * Two mechanisms, because the browser families genuinely differ:
 *
 * Gecko (Firefox, Zen, LibreWolf, …) installs per profile, as
 * `<profile>/extensions/<id>.xpi`. No root. Whether an *unsigned* XPI is
 * accepted depends on how the build was compiled: `xpinstall.signatures.
 * required` is only honoured where MOZ_REQUIRE_SIGNING was off. Release
 * Firefox enforces and will reject it; forks like Zen do not. This script
 * refuses rather than leaving a silently disabled add-on behind.
 *
 * Chromium (Chromium, Google Chrome) has no per-user external-extension
 * directory on Linux, so a persistent store-free install must write to a
 * system path and needs root. Run as root to have it done, or run as yourself
 * to be handed the exact commands.
 */

import { execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { chmod, copyFile, mkdir, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, '..');
const dist = join(repo, 'dist');
const HOME = homedir();

const args = process.argv.slice(2);
const DRY = args.includes('--dry-run');
const UNINSTALL = args.includes('--uninstall');
const only = args.filter((a) => !a.startsWith('--'));

const GECKO_ID = 'ratblocker@ratblocker.github.io';
const MARKER = '// added by RatBlocker install.mjs';

/* ------------------------------------------------------------------ Gecko */

/**
 * Gecko browsers and where they keep profiles. `enforcesSigning` records what
 * `tests/browser/gecko-signing.mjs` reports for that build; run it to check a
 * browser that is not listed here rather than assuming.
 */
const GECKO = [
  {
    name: 'firefox',
    binaries: ['firefox'],
    roots: [join(HOME, '.mozilla/firefox')],
    enforcesSigning: true,
  },
  {
    name: 'zen',
    binaries: [join(HOME, '.tarball-installations/zen/zen'), 'zen', 'zen-browser'],
    roots: [join(HOME, '.zen')],
    enforcesSigning: false,
  },
  {
    // A flatpak is a separate installation with its own profile tree. Pairing
    // the native binary with these profiles would install into a browser the
    // user is not launching.
    name: 'zen-flatpak',
    flatpak: 'app.zen_browser.zen',
    roots: [join(HOME, '.var/app/app.zen_browser.zen/.zen')],
    enforcesSigning: false,
  },
  {
    name: 'librewolf',
    binaries: ['librewolf'],
    roots: [join(HOME, '.librewolf')],
    enforcesSigning: false,
  },
  {
    name: 'librewolf-flatpak',
    flatpak: 'io.gitlab.librewolf-community',
    roots: [join(HOME, '.var/app/io.gitlab.librewolf-community/.librewolf')],
    enforcesSigning: false,
  },
  {
    name: 'waterfox',
    binaries: ['waterfox'],
    roots: [join(HOME, '.waterfox')],
    enforcesSigning: false,
  },
];

/** Is this flatpak application installed? */
function flatpakInstalled(appId) {
  try {
    const out = execFileSync('flatpak', ['list', '--app', '--columns=application'], {
      encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'],
    });
    return out.split('\n').some((l) => l.trim() === appId);
  } catch {
    return false;
  }
}

function onPath(candidate) {
  if (candidate.includes('/')) return existsSync(candidate) ? candidate : null;
  try {
    return execFileSync('which', [candidate], { encoding: 'utf8' }).trim() || null;
  } catch {
    return null;
  }
}

/** Parse profiles.ini into absolute profile directories. */
async function profilesIn(root) {
  const ini = join(root, 'profiles.ini');
  if (!existsSync(ini)) return [];
  const text = await readFile(ini, 'utf8');
  const found = [];
  let current = null;
  for (const raw of text.split('\n')) {
    const line = raw.trim();
    if (line.startsWith('[')) {
      if (current?.path) found.push(current);
      current = line.startsWith('[Profile') ? {} : null;
      continue;
    }
    if (current === null) continue;
    const eq = line.indexOf('=');
    if (eq < 0) continue;
    const key = line.slice(0, eq).trim();
    const value = line.slice(eq + 1).trim();
    if (key === 'Path') current.path = value;
    if (key === 'IsRelative') current.relative = value === '1';
    if (key === 'Default') current.default = value === '1';
  }
  if (current?.path) found.push(current);
  return found
    .map((p) => ({ ...p, dir: p.relative === false ? p.path : join(root, p.path) }))
    .filter((p) => existsSync(p.dir));
}

/** True when the XPI carries a Mozilla signature. */
async function xpiIsSigned(xpi) {
  const buf = await readFile(xpi);
  return buf.includes(Buffer.from('META-INF/mozilla.rsa'));
}

/**
 * Keep `xpinstall.signatures.required=false` in the profile's user.js.
 * Bounded by a marker so the user's own prefs are never rewritten.
 */
async function setUserPrefs(profileDir, { remove = false } = {}) {
  const path = join(profileDir, 'user.js');
  const existing = existsSync(path) ? await readFile(path, 'utf8') : '';
  const stripped = existing
    .split('\n')
    .filter((l) => !l.includes(MARKER))
    .join('\n')
    .replace(/\n{3,}/g, '\n\n');
  if (remove) {
    if (stripped.trim() === '') await rm(path, { force: true });
    else await writeFile(path, stripped.endsWith('\n') ? stripped : `${stripped}\n`);
    return;
  }
  const lines = [
    `user_pref("xpinstall.signatures.required", false); ${MARKER}`,
    `user_pref("extensions.autoDisableScopes", 0); ${MARKER}`,
  ];
  const body = `${stripped.trimEnd()}\n${stripped.trim() === '' ? '' : '\n'}${lines.join('\n')}\n`;
  await writeFile(path, body.trimStart());
}

async function installGecko(browser, xpi, signed) {
  const binary = browser.flatpak !== undefined
    ? (flatpakInstalled(browser.flatpak) ? `flatpak: ${browser.flatpak}` : null)
    : (browser.binaries.map(onPath).find(Boolean) ?? null);
  if (binary === null) return null;

  // Check this before profiles: a build that will reject the XPI should say so
  // regardless of whether it happens to have been run yet.
  if (!signed && browser.enforcesSigning && !UNINSTALL) {
    return { browser: browser.name, binary, status: 'refused',
      note: 'this build enforces Mozilla signing and would reject an unsigned XPI' };
  }

  const roots = browser.roots.filter((r) => existsSync(r));
  if (roots.length === 0) {
    return { browser: browser.name, binary, status: 'no-profile',
      note: 'installed, but has never been run — start it once to create a profile' };
  }

  const touched = [];
  for (const root of roots) {
    for (const profile of await profilesIn(root)) {
      const target = join(profile.dir, 'extensions', `${GECKO_ID}.xpi`);
      if (DRY) { touched.push(`${UNINSTALL ? 'would remove' : 'would install'} ${target}`); continue; }
      if (UNINSTALL) {
        await rm(target, { force: true });
        await setUserPrefs(profile.dir, { remove: true });
        touched.push(`removed ${target}`);
      } else {
        await mkdir(dirname(target), { recursive: true });
        await copyFile(xpi, target);
        await chmod(target, 0o644);
        if (!signed) await setUserPrefs(profile.dir);
        touched.push(`installed ${target}`);
      }
    }
  }
  return { browser: browser.name, binary, status: touched.length ? 'ok' : 'no-profile',
    actions: touched,
    note: touched.length ? 'restart the browser to pick it up' : 'profiles.ini lists no usable profile' };
}

/* --------------------------------------------------------------- Chromium */

const CHROMIUM = [
  { name: 'chromium', binaries: ['chromium', 'chromium-browser'],
    extensionDir: '/usr/share/chromium/extensions',
    policyDir: '/etc/chromium/policies/managed' },
  { name: 'google-chrome', binaries: ['google-chrome', 'google-chrome-stable'],
    extensionDir: '/opt/google/chrome/extensions',
    policyDir: '/etc/opt/chrome/policies/managed' },
];

const CRX_INSTALLED = '/usr/share/ratblocker/ratblocker-chromium.crx';

async function planChromium(browser, crx, descriptor, id) {
  const binary = browser.binaries.map(onPath).find(Boolean);
  if (binary === null || binary === undefined) return null;

  const target = join(browser.extensionDir, `${id}.json`);
  const isRoot = process.getuid?.() === 0;

  if (UNINSTALL) {
    const commands = [`rm -f ${target}`];
    if (browser.name === 'chromium') commands.push(`rm -f ${CRX_INSTALLED}`);
    if (isRoot && !DRY) {
      await rm(target, { force: true });
      if (browser.name === 'chromium') await rm(CRX_INSTALLED, { force: true });
      return { browser: browser.name, binary, status: 'ok', actions: commands.map((c) => `ran: ${c}`) };
    }
    return { browser: browser.name, binary, status: 'needs-root', commands };
  }

  const commands = [
    `install -Dm644 ${crx} ${CRX_INSTALLED}`,
    `install -Dm644 ${descriptor} ${target}`,
  ];
  if (isRoot && !DRY) {
    await mkdir(dirname(CRX_INSTALLED), { recursive: true });
    await copyFile(crx, CRX_INSTALLED);
    await chmod(CRX_INSTALLED, 0o644);
    await mkdir(browser.extensionDir, { recursive: true });
    await copyFile(descriptor, target);
    await chmod(target, 0o644);
    return { browser: browser.name, binary, status: 'ok',
      actions: commands.map((c) => `ran: ${c}`), note: 'restart the browser to pick it up' };
  }
  return { browser: browser.name, binary, status: 'needs-root', commands };
}

/* ------------------------------------------------------------------- main */

const xpi = (await readdir(dist).catch(() => []))
  .filter((f) => f.startsWith('ratblocker-firefox-') && f.endsWith('.xpi'))
  .sort()
  .pop();
const crx = join(dist, 'ratblocker-chromium.crx');

if (xpi === undefined && !existsSync(crx)) {
  console.error('Nothing packaged yet. Run: node build.mjs && node package.mjs');
  process.exit(1);
}

const xpiPath = xpi === undefined ? null : join(dist, xpi);
const signed = xpiPath === null ? false : await xpiIsSigned(xpiPath);

let descriptor = null;
let id = null;
if (existsSync(crx)) {
  const found = (await readdir(dist)).filter((f) => /^[a-p]{32}\.json$/.test(f));
  if (found.length > 0) { id = found[0].replace('.json', ''); descriptor = join(dist, found[0]); }
}

const results = [];
for (const browser of GECKO) {
  if (only.length > 0 && !only.includes(browser.name)) continue;
  if (xpiPath === null) continue;
  results.push(await installGecko(browser, xpiPath, signed));
}
for (const browser of CHROMIUM) {
  if (only.length > 0 && !only.includes(browser.name)) continue;
  if (descriptor === null) continue;
  results.push(await planChromium(browser, crx, descriptor, id));
}

const found = results.filter(Boolean);
console.log(`\nRatBlocker ${UNINSTALL ? 'uninstall' : 'install'}${DRY ? ' (dry run)' : ''}`);
console.log(`gecko XPI: ${xpiPath === null ? 'not packaged' : `${xpi} (${signed ? 'signed' : 'UNSIGNED'})`}`);
console.log(`chromium CRX: ${descriptor === null ? 'not packaged' : `${id}`}\n`);

if (found.length === 0) {
  console.log('No supported browser found on this machine.');
  process.exit(0);
}

const rootCommands = [];
for (const r of found) {
  const mark = { ok: '✓', refused: '✗', 'needs-root': '!', 'no-profile': '-' }[r.status] ?? '?';
  console.log(`${mark} ${r.browser.padEnd(15)} ${r.binary}`);
  for (const action of r.actions ?? []) console.log(`    ${action}`);
  if (r.status === 'needs-root') {
    console.log('    needs root; commands collected below');
    rootCommands.push(...r.commands);
  }
  if (r.note) console.log(`    ${r.note}`);
}

if (found.some((r) => r.status === 'refused')) {
  console.log('\nA refused browser enforces Mozilla signing. Either use a build that');
  console.log('does not (Zen, LibreWolf, Developer Edition, Nightly, ESR, unbranded),');
  console.log('or run sign-firefox.mjs to have Mozilla sign the XPI for unlisted');
  console.log('distribution. Check any build with:');
  console.log('  node tests/browser/gecko-signing.mjs <binary> <xpi>');
}

if (rootCommands.length > 0) {
  console.log('\nChromium-based browsers have no per-user external-extension directory,');
  console.log('so these need root:\n');
  console.log(`  sudo sh -c '${rootCommands.join(' && ')}'`);
}
