/**
 * Copy the shared design assets into the site's `public/` directory.
 *
 * The Angular builder refuses asset paths outside the workspace root, and the
 * icon and banner live at the repository root because the extensions and the
 * README use them too. Copying keeps one authoritative source without
 * duplicating it in version control — `public/design/` is generated and
 * gitignored.
 */

import { cp, mkdir } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, '../..');
const source = join(repo, 'design');
const target = join(here, '../public/design');

if (!existsSync(source)) {
  throw new Error(`design assets not found at ${source}`);
}

await mkdir(dirname(target), { recursive: true });
await cp(source, target, { recursive: true });
console.log(`copied design assets -> public/design`);
