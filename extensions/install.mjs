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
/**
 * Update mode: install only where what is already there is older.
 *
 * This is the whole of RatBlocker's "update engine" for every target that does
 * not require a server. Neither browser needs to be told: Gecko reads the
 * version out of <profile>/extensions/<id>.xpi at startup, and Chromium
 * rescans its external-extension descriptors at every start and upgrades when
 * external_version is higher. Replacing the file *is* the update. The browser
 * still performs the install itself, and still verifies it.
 */
const UPDATE = args.includes('--update');
const only = args.filter((a, i) => !a.startsWith('--') && args[i - 1] !== '--platform');

const GECKO_ID = 'ratblocker@ratblocker.github.io';
const MARKER = '// added by RatBlocker install.mjs';

/* ------------------------------------------------------------------ Gecko */

/**
 * Gecko browsers and where they keep profiles. `enforcesSigning` records what
 * `tests/browser/gecko-signing.mjs` reports for that build; run it to check a
 * browser that is not listed here rather than assuming.
 */
/**
 * `--platform <linux|darwin|win32>` reports what would happen on another
 * operating system. Only honoured with --dry-run: it skips the check that the
 * browser is actually present, because on a simulated platform it cannot be.
 * Useful for seeing the commands a Windows or macOS user will be given, and
 * for exercising those branches from a machine that is neither.
 */
const platformFlag = args.indexOf('--platform');
const PLATFORM_OVERRIDE =
  platformFlag >= 0 && DRY ? args[platformFlag + 1] : null;
const PLATFORM = PLATFORM_OVERRIDE ?? process.platform;
const SIMULATED = PLATFORM_OVERRIDE !== null && PLATFORM_OVERRIDE !== process.platform;
const APPDATA = process.env.APPDATA ?? join(HOME, 'AppData/Roaming');

/**
 * Gecko browsers: where the binary lives and where profiles are kept, per
 * platform. `enforcesSigning` records what `tests/browser/gecko-signing.mjs`
 * reports for that build; run it for anything not listed rather than guessing.
 */
const GECKO = [
  {
    name: 'firefox',
    enforcesSigning: true,
    binaries: {
      linux: ['firefox'],
      darwin: ['/Applications/Firefox.app/Contents/MacOS/firefox', 'firefox'],
      win32: [
        'C:\\Program Files\\Mozilla Firefox\\firefox.exe',
        'C:\\Program Files (x86)\\Mozilla Firefox\\firefox.exe',
      ],
    },
    roots: {
      linux: [join(HOME, '.mozilla/firefox')],
      darwin: [join(HOME, 'Library/Application Support/Firefox')],
      win32: [join(APPDATA, 'Mozilla/Firefox')],
    },
  },
  {
    name: 'zen',
    enforcesSigning: false,
    binaries: {
      linux: [join(HOME, '.tarball-installations/zen/zen'), 'zen', 'zen-browser'],
      darwin: ['/Applications/Zen Browser.app/Contents/MacOS/zen', '/Applications/Zen.app/Contents/MacOS/zen'],
      win32: [
        'C:\\Program Files\\Zen Browser\\zen.exe',
        join(process.env.LOCALAPPDATA ?? join(HOME, 'AppData/Local'), 'Zen Browser/zen.exe'),
      ],
    },
    roots: {
      linux: [join(HOME, '.zen')],
      darwin: [join(HOME, 'Library/Application Support/zen')],
      win32: [join(APPDATA, 'zen')],
    },
  },
  {
    // A flatpak is a separate installation with its own profile tree. Pairing
    // the native binary with these profiles would install into a browser the
    // user is not launching.
    name: 'zen-flatpak',
    enforcesSigning: false,
    flatpak: 'app.zen_browser.zen',
    roots: { linux: [join(HOME, '.var/app/app.zen_browser.zen/.zen')], darwin: [], win32: [] },
  },
  {
    name: 'librewolf',
    enforcesSigning: false,
    binaries: {
      linux: ['librewolf'],
      darwin: ['/Applications/LibreWolf.app/Contents/MacOS/librewolf'],
      win32: ['C:\\Program Files\\LibreWolf\\librewolf.exe'],
    },
    roots: {
      linux: [join(HOME, '.librewolf')],
      darwin: [join(HOME, 'Library/Application Support/librewolf')],
      win32: [join(APPDATA, 'librewolf')],
    },
  },
  {
    name: 'librewolf-flatpak',
    enforcesSigning: false,
    flatpak: 'io.gitlab.librewolf-community',
    roots: { linux: [join(HOME, '.var/app/io.gitlab.librewolf-community/.librewolf')], darwin: [], win32: [] },
  },
  {
    name: 'waterfox',
    enforcesSigning: false,
    binaries: {
      linux: ['waterfox'],
      darwin: ['/Applications/Waterfox.app/Contents/MacOS/waterfox'],
      win32: ['C:\\Program Files\\Waterfox\\waterfox.exe'],
    },
    roots: {
      linux: [join(HOME, '.waterfox')],
      darwin: [join(HOME, 'Library/Application Support/Waterfox')],
      win32: [join(APPDATA, 'Waterfox')],
    },
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

/**
 * What version is installed in this profile, according to the browser's own
 * database? Returns null when nothing is installed or the profile has not been
 * opened yet, which update mode treats as "install it".
 */
async function installedGeckoVersion(profileDir) {
  try {
    const db = JSON.parse(await readFile(join(profileDir, 'extensions.json'), 'utf8'));
    return (db.addons ?? []).find((a) => a.id === GECKO_ID)?.version ?? null;
  } catch {
    return null;
  }
}

/** Compare dotted version strings. Positive when `a` is newer than `b`. */
function compareVersions(a, b) {
  const pa = String(a).split('.').map((n) => Number.parseInt(n, 10) || 0);
  const pb = String(b).split('.').map((n) => Number.parseInt(n, 10) || 0);
  for (let i = 0; i < Math.max(pa.length, pb.length); i += 1) {
    const diff = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (diff !== 0) return diff;
  }
  return 0;
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

async function installGecko(browser, xpi, signed, packagedVersion) {
  const candidates = browser.binaries?.[PLATFORM] ?? [];
  const binary = browser.flatpak !== undefined
    ? (flatpakInstalled(browser.flatpak) ? `flatpak: ${browser.flatpak}` : null)
    : (candidates.map(onPath).find(Boolean)
       ?? (SIMULATED && candidates.length > 0 ? `${candidates[0]} (assumed)` : null));
  if (binary === null) return null;

  // Check this before profiles: a build that will reject the XPI should say so
  // regardless of whether it happens to have been run yet.
  if (!signed && browser.enforcesSigning && !UNINSTALL) {
    return { browser: browser.name, binary, status: 'refused',
      note: 'this build enforces Mozilla signing and would reject an unsigned XPI' };
  }

  const roots = (browser.roots?.[PLATFORM] ?? []).filter((r) => existsSync(r));
  if (roots.length === 0) {
    return { browser: browser.name, binary, status: 'no-profile',
      note: 'installed, but has never been run — start it once to create a profile' };
  }

  const touched = [];
  for (const root of roots) {
    for (const profile of await profilesIn(root)) {
      const target = join(profile.dir, 'extensions', `${GECKO_ID}.xpi`);

      if (UPDATE && !UNINSTALL) {
        const current = await installedGeckoVersion(profile.dir);
        if (current !== null && compareVersions(packagedVersion, current) <= 0) {
          touched.push(`up to date (${current}) in ${profile.dir}`);
          continue;
        }
        touched.push(`${current ?? 'nothing'} -> ${packagedVersion} in ${profile.dir}`);
        if (DRY) continue;
        await mkdir(dirname(target), { recursive: true });
        await copyFile(xpi, target);
        await chmod(target, 0o644);
        if (!signed) await setUserPrefs(profile.dir);
        continue;
      }

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

/**
 * Chromium-based browsers, and how a store-free install reaches each platform.
 *
 * These are not three spellings of one mechanism. On Linux an external-
 * extension descriptor installs a local CRX directly. On Windows and macOS,
 * Chrome refuses to install an external extension that is not hosted in the
 * Chrome Web Store, so the descriptor route does not exist there and
 * enterprise policy is the supported path — which means the CRX must be
 * reachable over HTTPS at RATBLOCKER_UPDATE_BASE, because policy tells the
 * browser where to fetch rather than handing it a file.
 */
const CHROMIUM = [
  {
    name: 'chromium',
    vendor: 'chromium',
    binaries: {
      linux: ['chromium', 'chromium-browser'],
      darwin: ['/Applications/Chromium.app/Contents/MacOS/Chromium'],
      win32: ['C:\\Program Files\\Chromium\\Application\\chrome.exe'],
    },
    linuxExtensionDir: '/usr/share/chromium/extensions',
    macPlistDomain: 'org.chromium.Chromium',
  },
  {
    name: 'google-chrome',
    vendor: 'chrome',
    binaries: {
      linux: ['google-chrome', 'google-chrome-stable'],
      darwin: ['/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'],
      win32: [
        'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
        'C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe',
      ],
    },
    linuxExtensionDir: '/opt/google/chrome/extensions',
    macPlistDomain: 'com.google.Chrome',
  },
];

const CRX_INSTALLED = '/usr/share/ratblocker/ratblocker-chromium.crx';

/** The update address baked into the packed build, if it has been packaged. */
async function chromiumUpdateUrl() {
  try {
    const manifest = JSON.parse(
      await readFile(join(here, 'chromium/build/manifest.json'), 'utf8'),
    );
    return manifest.update_url ?? null;
  } catch {
    return null;
  }
}

async function planChromium(browser, crx, descriptor, id) {
  const candidates = browser.binaries?.[PLATFORM] ?? [];
  const binary = candidates.map(onPath).find(Boolean)
    ?? (SIMULATED && candidates.length > 0 ? `${candidates[0]} (assumed)` : null);
  if (binary === null) return null;

  /**
   * Update mode is a no-op for an installed Chromium extension, and saying
   * "needs root" here would be wrong. Root is required to *install*, because
   * the external-extension directory is system-owned. Once installed the
   * browser updates the extension itself, from the update_url inside the CRX,
   * into the user's own profile — no privileges, and on Windows and macOS this
   * is the only mechanism there has ever been.
   */
  if (UPDATE && !UNINSTALL && PLATFORM === 'linux') {
    const target = join(browser.linuxExtensionDir, `${id}.json`);
    if (existsSync(target)) {
      let installedVersion = null;
      try {
        installedVersion = JSON.parse(await readFile(target, 'utf8')).external_version ?? null;
      } catch {
        // Unreadable descriptor: fall through and report it as installed.
      }
      const url = await chromiumUpdateUrl();
      return {
        browser: browser.name, binary, status: 'ok',
        actions: [`installed${installedVersion ? ` (${installedVersion})` : ''}`],
        note: url === null
          ? 'the browser updates this itself; no update_url is packaged, so package with RATBLOCKER_UPDATE_BASE set'
          : `the browser updates this itself from ${url} — nothing to do`,
      };
    }
    // Not installed: fall through to the install plan, which needs root.
  }

  const elevated = PLATFORM === 'win32' ? false : process.getuid?.() === 0;
  const policyDir = join(dist, 'policy');

  if (PLATFORM === 'linux') {
    const target = join(browser.linuxExtensionDir, `${id}.json`);
    const commands = UNINSTALL
      ? [`rm -f ${target}`, `rm -f ${CRX_INSTALLED}`]
      : [
          `install -Dm644 ${crx} ${CRX_INSTALLED}`,
          `install -Dm644 ${descriptor} ${target}`,
        ];
    if (elevated && !DRY) {
      if (UNINSTALL) {
        await rm(target, { force: true });
        await rm(CRX_INSTALLED, { force: true });
      } else {
        await mkdir(dirname(CRX_INSTALLED), { recursive: true });
        await copyFile(crx, CRX_INSTALLED);
        await chmod(CRX_INSTALLED, 0o644);
        await mkdir(browser.linuxExtensionDir, { recursive: true });
        await copyFile(descriptor, target);
        await chmod(target, 0o644);
      }
      return { browser: browser.name, binary, status: 'ok',
        actions: commands.map((c) => `ran: ${c}`), note: 'restart the browser' };
    }
    return { browser: browser.name, binary, status: 'needs-root', commands };
  }

  if (PLATFORM === 'darwin') {
    const source = join(policyDir, `macos-${browser.macPlistDomain}.plist`);
    const target = `/Library/Managed Preferences/${browser.macPlistDomain}.plist`;
    const commands = UNINSTALL
      ? [`rm -f "${target}"`]
      : [`install -m 644 "${source}" "${target}"`];
    return {
      browser: browser.name, binary,
      status: elevated && !DRY ? 'manual' : 'needs-root',
      commands,
      note:
        'Chrome refuses off-store external installs on macOS, so this goes through ' +
        'policy; the CRX must be served over HTTPS at RATBLOCKER_UPDATE_BASE. ' +
        'On a managed Mac, deploy the plist through MDM instead of writing it by hand.',
    };
  }

  if (PLATFORM === 'win32') {
    const source = join(policyDir, `windows-${browser.vendor}.reg`);
    const hive = browser.vendor === 'chrome'
      ? 'HKLM\\SOFTWARE\\Policies\\Google\\Chrome\\ExtensionSettings'
      : 'HKLM\\SOFTWARE\\Policies\\Chromium\\ExtensionSettings';
    const commands = UNINSTALL
      ? [`reg delete "${hive}\\${id}" /f`]
      : [`reg import "${source}"`];
    return {
      browser: browser.name, binary, status: 'needs-admin', commands,
      note:
        'Chrome refuses off-store external installs on Windows, so this goes through ' +
        'policy; run from an elevated prompt. The CRX must be served over HTTPS at ' +
        'RATBLOCKER_UPDATE_BASE.',
    };
  }

  return { browser: browser.name, binary, status: 'unsupported',
    note: `no store-free install route is implemented for ${PLATFORM}` };
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

// The version actually inside the packaged XPI, which is what would be
// installed. Read from the file rather than from package.json so the two can
// never disagree.
let packagedVersion = '0.0.0';
if (xpiPath !== null) {
  const match = /ratblocker-firefox-(.+)\.xpi$/.exec(xpi);
  if (match !== null) packagedVersion = match[1];
}

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
  results.push(await installGecko(browser, xpiPath, signed, packagedVersion));
}
for (const browser of CHROMIUM) {
  if (only.length > 0 && !only.includes(browser.name)) continue;
  if (descriptor === null) continue;
  results.push(await planChromium(browser, crx, descriptor, id));
}

const found = results.filter(Boolean);
const mode = UNINSTALL ? 'uninstall' : UPDATE ? 'update' : 'install';
console.log(`\nRatBlocker ${mode}${DRY ? ' (dry run)' : ''}`);
console.log(`gecko XPI: ${xpiPath === null ? 'not packaged' : `${xpi} (${signed ? 'signed' : 'UNSIGNED'})`}`);
console.log(`chromium CRX: ${descriptor === null ? 'not packaged' : `${id}`}\n`);

if (found.length === 0) {
  console.log('No supported browser found on this machine.');
  process.exit(0);
}

const rootCommands = [];
for (const r of found) {
  const mark = { ok: '✓', refused: '✗', 'needs-root': '!', 'needs-admin': '!',
                 manual: '!', 'no-profile': '-', unsupported: '✗' }[r.status] ?? '?';
  console.log(`${mark} ${r.browser.padEnd(15)} ${r.binary}`);
  for (const action of r.actions ?? []) console.log(`    ${action}`);
  if (r.status === 'needs-root' || r.status === 'needs-admin' || r.status === 'manual') {
    console.log(`    ${r.status === 'needs-admin' ? 'needs an elevated prompt' : 'needs root'};`
      + ' commands collected below');
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
  console.log('\nChromium-based browsers need elevated privileges for a store-free');
  console.log('install, because the location involved is system-wide:\n');
  const prefix = PLATFORM === 'win32' ? '' : 'sudo ';
  for (const command of rootCommands) console.log(`  ${prefix}${command}`);
  if (PLATFORM === 'win32') {
    console.log('\nRun these from an elevated Command Prompt or PowerShell.');
  }
}
