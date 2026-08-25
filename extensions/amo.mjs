/**
 * Shared helpers for talking to the addons.mozilla.org (AMO) API.
 *
 * Used by `sign-firefox.mjs` (submit a version) and `update-amo-listing.mjs`
 * (decorate the listing with an icon and a preview). Keeping the credentials
 * loader, the JWT mint, and the request wrapper in one place means the two
 * scripts cannot drift on how they authenticate.
 *
 * Credentials come from the environment, from `.env`, or from
 * `.amo-credentials` (both gitignored):
 *
 *   AMO_JWT_ISSUER=user:12345:67
 *   AMO_JWT_SECRET=...
 */
import { createHmac, randomUUID } from 'node:crypto';
import { existsSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const API = 'https://addons.mozilla.org/api/v5';
export const SITE = 'https://addons.mozilla.org';

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, '..');

/** KEY=VALUE lines, `#` comments, blank lines ignored. No quoting rules. */
async function loadDotEnv(file) {
  const map = {};
  if (!existsSync(file)) return map;
  for (const line of (await readFile(file, 'utf8')).split('\n')) {
    const trimmed = line.trim();
    if (trimmed === '' || trimmed.startsWith('#')) continue;
    const [key, ...rest] = trimmed.split('=');
    if (key !== undefined && rest.length > 0) map[key.trim()] = rest.join('=').trim();
  }
  return map;
}

/** Resolve AMO credentials from the environment, `.env`, or `.amo-credentials`. */
export async function credentials() {
  let issuer = process.env.AMO_JWT_ISSUER;
  let secret = process.env.AMO_JWT_SECRET;

  const envFile = await loadDotEnv(join(repo, '.env'));
  issuer ??= envFile.AMO_JWT_ISSUER;
  secret ??= envFile.AMO_JWT_SECRET;

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
        'Set AMO_JWT_ISSUER and AMO_JWT_SECRET, put them in .env, or in .amo-credentials.\n' +
        'Generate a key at https://addons.mozilla.org/developers/addon/api/key/',
    );
  }
  return { issuer, secret };
}

/** AMO authenticates with a short-lived HS256 JWT, one per request. */
export function token({ issuer, secret }) {
  const base64url = (obj) => Buffer.from(JSON.stringify(obj)).toString('base64url');
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

/** Authenticated JSON request. Throws with the response body on a non-2xx. */
export async function call(path, options = {}, creds) {
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
