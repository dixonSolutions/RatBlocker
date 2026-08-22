/**
 * Submit the Firefox XPI to addons.mozilla.org, in either channel.
 *
 *   node sign-firefox.mjs                    unlisted (the default)
 *   node sign-firefox.mjs --channel listed   the public AMO catalogue
 *
 * Why this exists: release Firefox refuses to permanently install an unsigned
 * extension, and the two channels answer that differently.
 *
 * *Unlisted* keeps distribution in your hands — Mozilla signs the file, you
 * host it and its updates yourself, there is no review queue and nothing
 * appears in the public directory. The signed XPI is downloaded here.
 *
 * *Listed* puts the add-on in the public catalogue, which is what gives it an
 * addons.mozilla.org page, a one-click install button, and updates served by
 * Mozilla. It goes through a review queue, so there is no signed file to
 * collect at the end of this script; what it writes instead is
 * `dist/amo-listing.json`, the listing address the site links to.
 *
 * Credentials come from the environment or from `.amo-credentials` (which is
 * gitignored):
 *
 *   AMO_JWT_ISSUER=user:12345:67
 *   AMO_JWT_SECRET=...
 *
 * Generate them at https://addons.mozilla.org/developers/addon/api/key/
 */

import { createHmac, randomUUID } from 'node:crypto';
import { existsSync } from 'node:fs';
import { readFile, writeFile, readdir } from 'node:fs/promises';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, '..');
const dist = join(repo, 'dist');

const API = 'https://addons.mozilla.org/api/v5';
const SITE = 'https://addons.mozilla.org';

const args = process.argv.slice(2);
const channelFlag = args.indexOf('--channel');
const CHANNEL = channelFlag >= 0 ? args[channelFlag + 1] : 'unlisted';
if (CHANNEL !== 'listed' && CHANNEL !== 'unlisted') {
  throw new Error(`--channel must be "listed" or "unlisted", not "${CHANNEL}"`);
}

/**
 * The listing metadata AMO requires before it will accept a public submission.
 * Only sent when the add-on is created; afterwards the listing is edited on
 * AMO itself, and this script must not quietly overwrite what is there.
 */
const LISTING = {
  slug: 'ratblocker',
  name: { 'en-US': 'RatBlocker' },
  summary: {
    'en-US':
      'Local, private ad and tracker blocking. Filtering happens on your machine: '
      + 'no account, no cloud service, no telemetry, and no TLS interception.',
  },
  categories: ['privacy-security'],
  license: 'GPL-3.0-or-later',
};
const DOWNLOAD_BASE =
  process.env.RATBLOCKER_UPDATE_BASE ?? 'https://ratblocker.example/downloads';

async function credentials() {
  let issuer = process.env.AMO_JWT_ISSUER;
  let secret = process.env.AMO_JWT_SECRET;

  const file = join(repo, '.amo-credentials');
  if ((issuer === undefined || secret === undefined) && existsSync(file)) {
    for (const line of (await readFile(file, 'utf8')).split('\n')) {
      const [key, ...rest] = line.split('=');
      const value = rest.join('=').trim();
      if (key.trim() === 'AMO_JWT_ISSUER') issuer ??= value;
      if (key.trim() === 'AMO_JWT_SECRET') secret ??= value;
    }
  }

  if (issuer === undefined || secret === undefined) {
    throw new Error(
      'AMO credentials not found.\n' +
        'Set AMO_JWT_ISSUER and AMO_JWT_SECRET, or put them in .amo-credentials.\n' +
        'Generate a key at https://addons.mozilla.org/developers/addon/api/key/',
    );
  }
  return { issuer, secret };
}

/** AMO authenticates with a short-lived HS256 JWT, one per request. */
function token({ issuer, secret }) {
  const base64url = (obj) =>
    Buffer.from(JSON.stringify(obj)).toString('base64url');
  const issued = Math.floor(Date.now() / 1000);
  const header = base64url({ alg: 'HS256', typ: 'JWT' });
  const payload = base64url({
    iss: issuer,
    jti: randomUUID(),
    iat: issued,
    // AMO rejects anything longer than five minutes.
    exp: issued + 270,
  });
  const signature = createHmac('sha256', secret)
    .update(`${header}.${payload}`)
    .digest('base64url');
  return `${header}.${payload}.${signature}`;
}

async function call(path, options = {}, creds) {
  const response = await fetch(`${API}${path}`, {
    ...options,
    headers: {
      Authorization: `JWT ${token(creds)}`,
      ...(options.headers ?? {}),
    },
  });
  const text = await response.text();
  if (!response.ok) {
    throw new Error(`${options.method ?? 'GET'} ${path} -> ${response.status}: ${text.slice(0, 500)}`);
  }
  return text === '' ? {} : JSON.parse(text);
}

async function latestXpi() {
  const files = (await readdir(dist))
    .filter((f) => f.startsWith('ratblocker-firefox-') && f.endsWith('.xpi') && !f.includes('signed'))
    .sort();
  const file = files.at(-1);
  if (file === undefined) {
    throw new Error('no XPI in dist/. Run `node package.mjs firefox` first.');
  }
  return join(dist, file);
}

async function main() {
  const creds = await credentials();
  const xpi = await latestXpi();
  const manifest = JSON.parse(await readFile(join(here, 'firefox/build/manifest.json'), 'utf8'));
  const guid = manifest.browser_specific_settings.gecko.id;
  const { version } = manifest;

  console.log(`signing ${basename(xpi)} as ${guid} ${version} (${CHANNEL})`);

  // 1. Upload.
  const form = new FormData();
  form.append('upload', new Blob([await readFile(xpi)]), basename(xpi));
  form.append('channel', CHANNEL);
  const upload = await call('/addons/upload/', { method: 'POST', body: form }, creds);
  console.log(`  upload ${upload.uuid}`);

  // 2. Wait for validation.
  let validated;
  for (let attempt = 0; attempt < 60; attempt += 1) {
    validated = await call(`/addons/upload/${upload.uuid}/`, {}, creds);
    if (validated.processed) break;
    await new Promise((r) => setTimeout(r, 5000));
  }
  if (validated?.processed !== true) throw new Error('validation timed out');
  if (validated.valid !== true) {
    const messages = (validated.validation?.messages ?? [])
      .filter((m) => m.type === 'error')
      .map((m) => `    ${m.message} ${JSON.stringify(m.description ?? '')}`)
      .join('\n');
    throw new Error(`AMO rejected the package:\n${messages || JSON.stringify(validated.validation).slice(0, 800)}`);
  }
  console.log('  validation passed');

  // 3. Create the version. The add-on may or may not exist yet.
  let versionRecord;
  let created;
  try {
    versionRecord = await call(
      `/addons/addon/${encodeURIComponent(guid)}/versions/`,
      {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ upload: upload.uuid }),
      },
      creds,
    );
  } catch (error) {
    if (!/404/.test(String(error))) throw error;
    console.log('  add-on not registered yet; creating it');
    created = await call(
      '/addons/addon/',
      {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(
          CHANNEL === 'listed'
            ? { upload: upload.uuid, ...LISTING }
            : { upload: upload.uuid },
        ),
      },
      creds,
    );
    versionRecord = created.version ?? created;
  }

  if (CHANNEL === 'listed') {
    // A listed version goes into a review queue, so there is no signed file to
    // wait for here. What matters to everything downstream is the address the
    // add-on now lives at, which is what the site's install button points to.
    const addon = created ?? await call(`/addons/addon/${encodeURIComponent(guid)}/`, {}, creds);
    const slug = addon.slug ?? LISTING.slug;
    const listing = {
      guid,
      version,
      channel: 'listed',
      slug,
      listingUrl: `${SITE}/firefox/addon/${slug}/`,
      latestXpiUrl: `${SITE}/firefox/downloads/latest/${slug}/latest.xpi`,
      submittedAt: new Date().toISOString(),
    };
    await writeFile(join(dist, 'amo-listing.json'), `${JSON.stringify(listing, null, 2)}\n`);
    console.log(`  listing -> ${listing.listingUrl}`);
    console.log('  written to dist/amo-listing.json');
    console.log('\nThe version is queued for review. Once it is approved, the listing');
    console.log('page and the install button on the site both work, and Firefox');
    console.log('updates every install from Mozilla with nothing to host.');
    return;
  }

  // 4. Wait for the signed file.
  let signedUrl;
  for (let attempt = 0; attempt < 60; attempt += 1) {
    const detail = await call(
      `/addons/addon/${encodeURIComponent(guid)}/versions/${versionRecord.id}/`,
      {},
      creds,
    );
    if (detail.file?.url !== undefined && detail.file.status !== 'unreviewed') {
      signedUrl = detail.file.url;
      break;
    }
    if (detail.file?.url !== undefined) signedUrl = detail.file.url;
    if (signedUrl !== undefined) break;
    await new Promise((r) => setTimeout(r, 5000));
  }
  if (signedUrl === undefined) throw new Error('signing timed out');

  // 5. Download it.
  const signed = await fetch(signedUrl, {
    headers: { Authorization: `JWT ${token(creds)}` },
  });
  if (!signed.ok) throw new Error(`downloading the signed XPI failed: ${signed.status}`);
  const out = join(dist, `ratblocker-firefox-${version}-signed.xpi`);
  await writeFile(out, Buffer.from(await signed.arrayBuffer()));
  console.log(`  signed XPI -> ${out}`);

  // 6. The update manifest Firefox polls, for self-hosted updates.
  const updates = {
    addons: {
      [guid]: {
        updates: [
          {
            version,
            update_link: `${DOWNLOAD_BASE}/ratblocker-firefox-${version}-signed.xpi`,
          },
        ],
      },
    },
  };
  await writeFile(
    join(dist, 'firefox-updates.json'),
    `${JSON.stringify(updates, null, 2)}\n`,
  );
  console.log('  update manifest -> dist/firefox-updates.json');
  console.log('\nHost both files at the URL in the manifest\'s update_url and');
  console.log('Firefox will install and update from there, with no store listing.');
}

await main();
