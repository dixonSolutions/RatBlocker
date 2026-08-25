/**
 * Decorate the RatBlocker AMO listing with the brand banner.
 *
 *   node update-amo-listing.mjs
 *
 * The icon is already carried by the XPI manifest, so AMO shows it with no
 * extra work. AMO has no "banner" slot, but it does have preview screenshots,
 * so the banner from `design/` is uploaded as the first preview — which is what
 * gives the listing a hero image.
 *
 * Run this *after* a listed publish has created the add-on. It is idempotent in
 * the sense that re-running it adds another preview; delete extras on AMO if
 * you need to. Credentials are loaded the same way as `sign-firefox.mjs`.
 */
import { readFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { credentials, call } from './amo.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, '..');
const dist = join(repo, 'dist');
// AMO prefers 1280x800 screenshots; the banner is 1280x320, so the centred
// preview is used when available. See design/README.md.
const preview1280 = join(repo, 'design', 'preview-1280x800.png');
const banner = join(repo, 'design', 'banner.png');
const preview = existsSync(preview1280) ? preview1280 : banner;

/** The add-on identifier AMO knows it by: the slug if a listing was recorded,
 * otherwise the gecko id from the manifest. */
async function addonId() {
  const listing = join(dist, 'amo-listing.json');
  if (existsSync(listing)) {
    const data = JSON.parse(await readFile(listing, 'utf8'));
    if (data.slug) return data.slug;
    if (data.guid) return data.guid;
  }
  const manifest = JSON.parse(
    await readFile(join(here, 'firefox/manifest.json'), 'utf8'),
  );
  return manifest.browser_specific_settings.gecko.id;
}

async function main() {
  if (!existsSync(preview)) {
    throw new Error(`preview not found: ${preview}`);
  }
  const creds = await credentials();
  const id = await addonId();
  console.log(`uploading preview for ${id}`);

  const form = new FormData();
  form.append('image', new Blob([await readFile(preview)]), 'preview.png');
  form.append('position', '1');
  const preview = await call(
    `/addons/addon/${encodeURIComponent(id)}/previews/`,
    { method: 'POST', body: form },
    creds,
  );
  console.log('  uploaded preview', preview.id);
  console.log(`  ${preview.image_url ?? '(image url pending async resize)'}`);
}

await main();
