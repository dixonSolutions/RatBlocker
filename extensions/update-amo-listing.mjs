/**
 * Decorate the RatBlocker AMO listing: the store icon and the banner preview.
 *
 *   node update-amo-listing.mjs
 *
 * Two uploads, both things AMO will not take from the XPI on their own:
 *
 *   - The **listing icon** is the square mark AMO shows in search results and on
 *     the add-on page. AMO exposes it as a separate `icon` field on the add-on
 *     (PATCH /addons/addon/<id>/), resized to 32/64/128. The XPI manifest
 *     `icons` only cover the in-browser toolbar/management page, so without
 *     this upload the AMO listing falls back to the generic puzzle piece.
 *   - The **banner** has no AMO slot, but preview screenshots do, so the
 *     1280x800 preview from `design/` is uploaded as the first preview — the
 *     listing's hero image.
 *
 * Both are idempotent: the icon PATCH replaces, and the preview step deletes
 * any existing previews before uploading, so this is safe to run from CI on
 * every release. Run it *after* a listed publish has created the add-on.
 * Credentials are loaded the same way as `sign-firefox.mjs`.
 */
import { readFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { credentials, call } from './amo.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, '..');
const dist = join(repo, 'dist');
// AMO recommends a square PNG icon and resizes it to 32/64/128, so the
// highest-resolution square source gives the sharpest small sizes.
const icon = join(repo, 'design', 'icon-512.png');
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

/** PATCH the listing icon. AMO resizes it asynchronously to 32/64/128, so the
 * returned icon_url may lag by a moment. Replacing is idempotent. */
async function uploadIcon(id, creds) {
  const form = new FormData();
  form.append('icon', new Blob([await readFile(icon)]), 'icon-512.png');
  const result = await call(
    `/addons/addon/${encodeURIComponent(id)}/`,
    { method: 'PATCH', body: form },
    creds,
  );
  console.log('  icon uploaded', result.icon_url ?? '(pending async resize)');
}

/** Replace the banner preview so re-runs do not stack up duplicates: delete
 * every existing preview, then upload the current one at position 1. */
async function replacePreview(id, creds) {
  const addon = await call(`/addons/addon/${encodeURIComponent(id)}/`, {}, creds);
  for (const p of addon.previews ?? []) {
    await call(
      `/addons/addon/${encodeURIComponent(id)}/previews/${p.id}/`,
      { method: 'DELETE' },
      creds,
    );
    console.log(`  deleted old preview ${p.id}`);
  }

  const form = new FormData();
  form.append('image', new Blob([await readFile(preview)]), 'preview.png');
  form.append('position', '1');
  const result = await call(
    `/addons/addon/${encodeURIComponent(id)}/previews/`,
    { method: 'POST', body: form },
    creds,
  );
  console.log('  uploaded preview', result.id);
  console.log(`  ${result.image_url ?? '(image url pending async resize)'}`);
}

async function main() {
  if (!existsSync(icon)) throw new Error(`icon not found: ${icon}`);
  if (!existsSync(preview)) throw new Error(`preview not found: ${preview}`);
  const creds = await credentials();
  const id = await addonId();
  console.log(`decorating AMO listing for ${id}`);
  await uploadIcon(id, creds);
  await replacePreview(id, creds);
}

await main();
