/**
 * Bump the extension version one patch step and keep every place that records it
 * in sync: both browser manifests, the npm package, and the Cargo workspace.
 *
 *   node bump-version.mjs            bump patch (0.1.4 -> 0.1.5)
 *   node bump-version.mjs 0.2.0      set an explicit version
 *
 * The Firefox manifest is the source of truth for what AMO receives, so it is
 * read first and the others are made to agree. Prints the new version to stdout
 * (last line) so a workflow can capture it with `::set-output`/`$GITHUB_ENV`.
 */
import { readFile, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, '..');

async function readJson(path) {
  return JSON.parse(await readFile(path, 'utf8'));
}

function bumpPatch(version) {
  const parts = version.split('.');
  if (parts.length !== 3 || parts.some((p) => !/^\d+$/.test(p))) {
    throw new Error(`"${version}" is not a major.minor.patch version`);
  }
  parts[2] = String(Number(parts[2]) + 1);
  return parts.join('.');
}

async function writeJson(path, data) {
  await writeFile(path, `${JSON.stringify(data, null, 2)}\n`);
}

async function main() {
  const requested = process.argv[2];
  const firefoxManifestPath = join(here, 'firefox/manifest.json');
  const firefox = await readJson(firefoxManifestPath);
  const next = requested ?? bumpPatch(firefox.version);
  if (!/^\d+\.\d+\.\d+$/.test(next)) {
    throw new Error(`invalid version "${next}"`);
  }

  // 1. Firefox manifest — the version AMO sees.
  firefox.version = next;
  await writeJson(firefoxManifestPath, firefox);

  // 2. Chromium manifest — kept in sync so the two stores never drift.
  const chromiumManifestPath = join(here, 'chromium/manifest.json');
  const chromium = await readJson(chromiumManifestPath);
  chromium.version = next;
  await writeJson(chromiumManifestPath, chromium);

  // 3. The npm package — informational, but drift here is confusing.
  const packagePath = join(here, 'package.json');
  const pkg = await readJson(packagePath);
  pkg.version = next;
  await writeJson(packagePath, pkg);

  // 4. The Cargo workspace version. Only the standalone `version = "..."` key
  // under [workspace.package] is touched; dependency versions inside `{ }` are
  // left alone.
  const cargoPath = join(repo, 'Cargo.toml');
  const cargo = await readFile(cargoPath, 'utf8');
  const replaced = cargo.replace(
    /(\[workspace\.package\][\s\S]*?version\s*=\s*)"[^"]*"/,
    `$1"${next}"`,
  );
  if (replaced === cargo) {
    throw new Error('could not find [workspace.package] version in Cargo.toml');
  }
  await writeFile(cargoPath, replaced);

  console.log(`version ${next}`);
}

await main();
