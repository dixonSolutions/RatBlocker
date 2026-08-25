/**
 * Package both extensions for self-hosted ("remote") distribution.
 *
 * Usage: `node package.mjs [chromium|firefox]`
 *
 * Chromium: produces a CRX3 signed with a key you keep, the update manifest a
 * browser polls, and the Linux external-extension descriptor that installs it
 * without the Web Store. The extension id is derived from the signing key, so
 * it stays the same across releases as long as the key does — which is what
 * makes updates work at all.
 *
 * Firefox: produces an unsigned XPI. Release Firefox will not permanently
 * install that, so `sign-firefox.mjs` submits it to addons.mozilla.org for
 * unlisted signing; see `docs/browser-support.md`.
 */

import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync } from 'node:fs';
import { mkdir, readFile, readdir, writeFile, stat } from 'node:fs/promises';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, '..');
const dist = join(repo, 'dist');

/** Where a browser will poll for updates. Override for real hosting. */
const UPDATE_BASE =
  process.env.RATBLOCKER_UPDATE_BASE ?? 'https://ratblocker.example/downloads';

/** Locate the Chromium binary used for packing. */
function chromiumBinary() {
  for (const candidate of ['chromium', 'chromium-browser', 'google-chrome']) {
    try {
      execFileSync('which', [candidate], { stdio: 'ignore' });
      return candidate;
    } catch {
      // Try the next one.
    }
  }
  throw new Error('no chromium binary found; install chromium to pack a CRX');
}

/**
 * Chrome derives an extension id from the SHA-256 of the DER public key:
 * the first 16 bytes, each nibble mapped from 0-f onto a-p.
 */
function extensionId(publicKeyDer) {
  const digest = createHash('sha256').update(publicKeyDer).digest();
  return [...digest.subarray(0, 16)]
    .map((byte) => byte.toString(16).padStart(2, '0'))
    .join('')
    .split('')
    .map((c) => String.fromCharCode('a'.charCodeAt(0) + parseInt(c, 16)))
    .join('');
}

/** Read the public key out of a CRX3 header. */
async function publicKeyFromCrx(crxPath) {
  const buf = await readFile(crxPath);
  if (buf.subarray(0, 4).toString('ascii') !== 'Cr24') {
    throw new Error(`${crxPath} is not a CRX file`);
  }
  const version = buf.readUInt32LE(4);
  if (version !== 3) throw new Error(`expected CRX3, got version ${version}`);
  const headerLength = buf.readUInt32LE(8);
  const header = buf.subarray(12, 12 + headerLength);

  // CrxFileHeader.sha256_with_rsa is field 2 (repeated AsymmetricKeyProof);
  // within that, public_key is field 1. Walk the protobuf rather than pulling
  // in a full decoder for two fields.
  let offset = 0;
  const readVarint = () => {
    let result = 0;
    let shift = 0;
    for (;;) {
      const byte = header[offset++];
      result |= (byte & 0x7f) << shift;
      if ((byte & 0x80) === 0) return result;
      shift += 7;
    }
  };
  while (offset < header.length) {
    const tag = readVarint();
    const field = tag >> 3;
    const wire = tag & 0x07;
    if (wire !== 2) throw new Error(`unexpected wire type ${wire} in CRX header`);
    const length = readVarint();
    const value = header.subarray(offset, offset + length);
    offset += length;
    if (field === 2 || field === 3) {
      // AsymmetricKeyProof { bytes public_key = 1; bytes signature = 2; }
      let inner = 0;
      const innerVarint = () => {
        let result = 0;
        let shift = 0;
        for (;;) {
          const byte = value[inner++];
          result |= (byte & 0x7f) << shift;
          if ((byte & 0x80) === 0) return result;
          shift += 7;
        }
      };
      const innerTag = innerVarint();
      const innerLength = innerVarint();
      if (innerTag >> 3 === 1) return value.subarray(inner, inner + innerLength);
    }
  }
  throw new Error('no public key found in the CRX header');
}

async function packChromium() {
  const build = join(here, 'chromium/build');
  if (!existsSync(build)) throw new Error(`build first: ${build} does not exist`);

  await mkdir(dist, { recursive: true });

  // The CRX must carry the address it will be updated from. Without an
  // update_url Chromium assumes the Web Store, and an extension that is not
  // there simply never updates. This is injected at packaging time rather than
  // kept in the source manifest because it is a distribution concern: a
  // development build loaded unpacked has no business polling anything.
  const manifestPath = join(build, 'manifest.json');
  const buildManifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  buildManifest.update_url = `${UPDATE_BASE}/chromium-update.xml`;
  await writeFile(manifestPath, `${JSON.stringify(buildManifest, null, 2)}\n`);
  const key = join(repo, 'chromium-signing-key.pem');
  const isNewKey = !existsSync(key);

  const args = [`--pack-extension=${build}`];
  if (!isNewKey) args.push(`--pack-extension-key=${key}`);
  execFileSync(chromiumBinary(), args, { stdio: 'inherit' });

  // Chromium writes `<dir>.crx` and, for a new key, `<dir>.pem` beside it.
  const producedCrx = join(here, 'chromium/build.crx');
  const producedKey = join(here, 'chromium/build.pem');
  if (isNewKey && existsSync(producedKey)) {
    const { rename } = await import('node:fs/promises');
    await rename(producedKey, key);
    console.log(`generated a new signing key at ${relative(repo, key)}`);
    console.log('KEEP THIS FILE. The extension id is derived from it; lose it');
    console.log('and every existing install stops receiving updates.');
  }

  const crx = join(dist, 'ratblocker-chromium.crx');
  const { rename } = await import('node:fs/promises');
  await rename(producedCrx, crx);

  const id = extensionId(await publicKeyFromCrx(crx));
  const manifest = JSON.parse(await readFile(join(build, 'manifest.json'), 'utf8'));
  const { version } = manifest;
  const size = (await stat(crx)).size;

  // The manifest a browser polls to discover a new version.
  const updateXml = `<?xml version="1.0" encoding="UTF-8"?>
<gupdate xmlns="http://www.google.com/update2/response" protocol="2.0">
  <app appid="${id}">
    <updatecheck codebase="${UPDATE_BASE}/ratblocker-chromium.crx" version="${version}" />
  </app>
</gupdate>
`;
  await writeFile(join(dist, 'chromium-update.xml'), updateXml);

  // The Linux external-extension descriptor: dropping this in
  // /usr/share/chromium/extensions installs the CRX with no store involved.
  await writeFile(
    join(dist, `${id}.json`),
    `${JSON.stringify(
      {
        external_crx: '/usr/share/ratblocker/ratblocker-chromium.crx',
        external_version: version,
      },
      null,
      2,
    )}\n`,
  );

  // The same id expressed as enterprise policy, per platform.
  //
  // This is not merely an alternative to the external-extension descriptor: on
  // Windows and macOS, Chrome refuses to install an external extension that is
  // not hosted in the Web Store, so policy is the *only* store-free route
  // there. Each operating system reads policy from somewhere different and in
  // a different format, so each gets its own artefact.
  const updateUrl = `${UPDATE_BASE}/chromium-update.xml`;
  const settings = {
    installation_mode: 'normal_installed',
    update_url: updateUrl,
    toolbar_pin: 'force_pinned',
  };
  const policyDir = join(dist, 'policy');
  await mkdir(policyDir, { recursive: true });

  // Linux: a JSON file dropped into the managed-policy directory.
  const policyJson = `${JSON.stringify({ ExtensionSettings: { [id]: settings } }, null, 2)}\n`;
  await writeFile(join(dist, 'chromium-policy.json'), policyJson);
  await writeFile(join(policyDir, 'linux-policy.json'), policyJson);

  // Windows: registry values under the policy hive. ExtensionSettings is
  // expressed as one subkey per extension id, each value a REG_SZ.
  const regEscape = (value) => value.replace(/\\/g, '\\\\').replace(/"/g, '\\"');
  for (const [vendor, hive] of [
    ['chrome', 'SOFTWARE\\Policies\\Google\\Chrome'],
    ['chromium', 'SOFTWARE\\Policies\\Chromium'],
  ]) {
    const lines = [
      'Windows Registry Editor Version 5.00',
      '',
      '; RatBlocker, installed from a host you control rather than the Web',
      '; Store. Chrome on Windows refuses off-store external extensions, so',
      '; policy is the supported route. Import with:  reg import <this file>',
      '; (an elevated prompt), then restart the browser.',
      '',
      `[HKEY_LOCAL_MACHINE\\${hive}\\ExtensionSettings]`,
      '',
      `[HKEY_LOCAL_MACHINE\\${hive}\\ExtensionSettings\\${id}]`,
      `"installation_mode"="${regEscape(settings.installation_mode)}"`,
      `"update_url"="${regEscape(settings.update_url)}"`,
      `"toolbar_pin"="${regEscape(settings.toolbar_pin)}"`,
      '',
    ];
    await writeFile(join(policyDir, `windows-${vendor}.reg`), lines.join('\r\n'));
  }

  // macOS: a managed-preferences property list.
  const plistEntries = Object.entries(settings)
    .map(([k, v]) => `        <key>${k}</key>\n        <string>${v}</string>`)
    .join('\n');
  for (const [vendor, domain] of [
    ['chrome', 'com.google.Chrome'],
    ['chromium', 'org.chromium.Chromium'],
  ]) {
    await writeFile(
      join(policyDir, `macos-${domain}.plist`),
      `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!--
  RatBlocker for ${vendor} on macOS. Chrome refuses off-store external
  extensions here, so policy is the supported route. Deploy through MDM, or
  for a single machine copy to /Library/Managed Preferences/${domain}.plist
  and restart the browser.
-->
<plist version="1.0">
  <dict>
    <key>ExtensionSettings</key>
    <dict>
      <key>${id}</key>
      <dict>
${plistEntries}
      </dict>
    </dict>
  </dict>
</plist>
`,
    );
  }

  // The remote variant: Chromium fetches the CRX named by the update manifest
  // and keeps it current, with nothing installed locally. `external_crx` and
  // `external_update_url` are alternatives, so they ship as separate files
  // rather than one ambiguous descriptor.
  await writeFile(
    join(dist, `${id}.update.json`),
    `${JSON.stringify(
      { external_update_url: `${UPDATE_BASE}/chromium-update.xml` },
      null,
      2,
    )}\n`,
  );

  console.log(`\nchromium extension id  ${id}`);
  console.log(`crx                    ${relative(repo, crx)} (${(size / 1024 / 1024).toFixed(1)} MiB)`);
  console.log(`update manifest        dist/chromium-update.xml`);
  console.log(`local install          dist/${id}.json -> /usr/share/chromium/extensions/`);
  console.log(`remote install         dist/${id}.update.json -> same directory`);
  console.log(`policy (linux)         dist/policy/linux-policy.json`);
  console.log(`policy (windows)       dist/policy/windows-{chrome,chromium}.reg`);
  console.log(`policy (macos)         dist/policy/macos-*.plist`);
  console.log(`update url             ${UPDATE_BASE}/chromium-update.xml`);
  return id;
}

/** Zip a directory into an XPI, which is just a zip with a manifest inside. */
async function packFirefox() {
  const build = join(here, 'firefox/build');
  if (!existsSync(build)) throw new Error(`build first: ${build} does not exist`);
  await mkdir(dist, { recursive: true });

  const manifestPath = join(build, 'manifest.json');
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));

  // Same reasoning as Chromium: where to check for updates is a property of
  // the distribution, not of the source tree. AMO forbids `update_url` on
  // listed (Mozilla-hosted) add-ons — it serves updates itself — so the field
  // is injected only for self-hosted (unlisted) packaging.
  const updateManifestUrl = `${UPDATE_BASE}/firefox-updates.json`;
  const selfHosted = process.env.RATBLOCKER_CHANNEL !== 'listed';
  if (selfHosted) {
    manifest.browser_specific_settings.gecko.update_url = updateManifestUrl;
  } else {
    delete manifest.browser_specific_settings.gecko.update_url;
  }
  await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);

  const xpi = join(dist, `ratblocker-firefox-${manifest.version}.xpi`);

  // Deterministic ordering keeps the archive reproducible.
  const entries = [];
  const walk = async (dir) => {
    for (const entry of (await readdir(dir, { withFileTypes: true })).sort((a, b) =>
      a.name.localeCompare(b.name),
    )) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) await walk(path);
      else entries.push(relative(build, path));
    }
  };
  await walk(build);

  execFileSync('zip', ['-q', '-X', '-9', xpi, ...entries], { cwd: build });
  const size = (await stat(xpi)).size;

  // The manifest Gecko polls. `update_link` must be https: Firefox refuses to
  // fetch an update over plain http regardless of the signature on the file.
  const id = manifest.browser_specific_settings.gecko.id;
  await writeFile(
    join(dist, 'firefox-updates.json'),
    `${JSON.stringify(
      {
        addons: {
          [id]: {
            updates: [
              {
                version: manifest.version,
                update_link: `${UPDATE_BASE}/ratblocker-firefox-${manifest.version}.xpi`,
              },
            ],
          },
        },
      },
      null,
      2,
    )}\n`,
  );
  console.log(`\nfirefox xpi            ${relative(repo, xpi)} (${(size / 1024 / 1024).toFixed(1)} MiB)`);
  console.log(`extension id           ${id}`);
  console.log(`update manifest        dist/firefox-updates.json`);
  console.log(`update url             ${updateManifestUrl}`);
  console.log('This XPI is unsigned. Release Firefox will only install it');
  console.log('temporarily; run sign-firefox.mjs to have Mozilla sign it for');
  console.log('self-hosted (unlisted) distribution.');
  return xpi;
}

const targets = process.argv.slice(2);
const wanted = targets.length > 0 ? targets : ['chromium', 'firefox'];
for (const target of wanted) {
  if (target === 'chromium') await packChromium();
  else if (target === 'firefox') await packFirefox();
  else throw new Error(`unknown target ${target}`);
}
