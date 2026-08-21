/**
 * Build both extensions from one shared source tree.
 *
 * Usage: `node build.mjs [chromium|firefox]` — with no argument, both.
 *
 * Everything the extensions load at runtime is produced upstream: the WASM
 * core by cargo, the rule database and rulesets by `ratblocker-compile`. This
 * script bundles the TypeScript and assembles the two packages, and it fails
 * loudly if an upstream artefact is missing rather than shipping a broken one.
 */

import { build } from 'esbuild';
import { cp, mkdir, readFile, rm, writeFile, readdir } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, '..');
const dist = join(repo, 'dist');
const wasmSource = join(
  repo,
  'target/wasm32-unknown-unknown/release/ratblocker_wasm.wasm',
);

const TARGETS = {
  chromium: {
    entries: {
      background: 'chromium/src/background.ts',
      content: 'shared/src/content.ts',
      'streaming-ads': 'shared/src/streaming-ads.ts',
    },
    // Chromium filters declaratively, so it only needs the cosmetic database.
    database: [join(dist, 'chromium/cosmetic.rbdb'), 'rules/cosmetic.rbdb'],
    rulesets: true,
    // Chromium filters through declarativeNetRequest, so the WASM engine only
    // ever sees cosmetic rules. Asking it how many rules are loaded would
    // report a number two orders of magnitude below what is actually
    // enforcing, so the real figures are baked in here instead.
    counts: (metadata) => ({
      networkRules: metadata.chromium.kept,
      networkSource: 'declarativeNetRequest',
      cosmeticRules: metadata.rules.cosmetic,
      collapsedDomains: metadata.chromium.collapsed_domains,
    }),
    esbuildTarget: 'chrome121',
  },
  firefox: {
    entries: {
      background: 'firefox/src/background.ts',
      content: 'shared/src/content.ts',
      'streaming-ads': 'shared/src/streaming-ads.ts',
    },
    // Firefox runs the full core, so it ships the full database.
    database: [join(dist, 'rules.rbdb'), 'rules/rules.rbdb'],
    rulesets: false,
    counts: (metadata) => ({
      networkRules: metadata.rules.network + metadata.rules.exceptions,
      networkSource: 'webRequest',
      cosmeticRules: metadata.rules.cosmetic,
      collapsedDomains: 0,
    }),
    esbuildTarget: 'firefox128',
  },
};

function required(path, what) {
  if (!existsSync(path)) {
    throw new Error(
      `missing ${what}: ${path}\n` +
        'Run `cargo build --release -p ratblocker-wasm --target wasm32-unknown-unknown` ' +
        'and `ratblocker-compile build ... --out dist` first.',
    );
  }
  return path;
}

async function buildTarget(name) {
  const target = TARGETS[name];
  if (target === undefined) throw new Error(`unknown target ${name}`);

  const out = join(here, name, 'build');
  await rm(out, { recursive: true, force: true });
  await mkdir(join(out, 'ui'), { recursive: true });
  await mkdir(join(out, 'core'), { recursive: true });
  await mkdir(join(out, 'rules'), { recursive: true });

  // 1. Bundle the background and content scripts.
  for (const [entryName, entry] of Object.entries(target.entries)) {
    await build({
      entryPoints: [join(here, entry)],
      outfile: join(out, `${entryName}.js`),
      bundle: true,
      format: 'esm',
      target: target.esbuildTarget,
      platform: 'browser',
      sourcemap: false,
      minify: false,
      logLevel: 'warning',
    });
  }

  // 2. Bundle the shared UI.
  for (const page of ['popup', 'options']) {
    await build({
      entryPoints: [join(here, `shared/ui/${page}.ts`)],
      outfile: join(out, `ui/${page}.js`),
      bundle: true,
      format: 'esm',
      target: target.esbuildTarget,
      platform: 'browser',
      logLevel: 'warning',
    });
    await cp(join(here, `shared/ui/${page}.html`), join(out, `ui/${page}.html`));
  }
  await cp(join(here, 'shared/ui/ui.css'), join(out, 'ui/ui.css'));

  // 3. Static assets and compiled artefacts.
  await cp(join(here, 'shared/icons'), join(out, 'icons'), { recursive: true });
  await cp(required(wasmSource, 'WebAssembly core'), join(out, 'core/ratblocker.wasm'));

  const [dbSource, dbDest] = target.database;
  await cp(required(dbSource, 'compiled rule database'), join(out, dbDest));

  await cp(
    required(join(dist, 'redirects'), 'redirect stand-in resources'),
    join(out, 'redirects'),
    { recursive: true },
  );

  // 4. Manifest, with the Chromium ruleset list reconciled against what the
  //    compiler actually produced.
  const manifest = JSON.parse(await readFile(join(here, name, 'manifest.json'), 'utf8'));

  if (target.rulesets) {
    const rulesetDir = required(join(dist, 'chromium'), 'Chromium rulesets');
    const files = (await readdir(rulesetDir))
      .filter((f) => /^ruleset_\d+\.json$/.test(f))
      .sort();
    if (files.length === 0) throw new Error('no Chromium rulesets were produced');
    await mkdir(join(out, 'rules'), { recursive: true });
    for (const file of files) {
      await cp(join(rulesetDir, file), join(out, 'rules', file));
    }
    manifest.declarative_net_request = {
      rule_resources: files.map((file) => ({
        id: file.replace('.json', ''),
        enabled: true,
        path: `rules/${file}`,
      })),
    };
  }

  await writeFile(join(out, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`);

  // 5. Rule counts, so the popup can state what is actually enforcing.
  const metadata = JSON.parse(
    await readFile(required(join(dist, 'metadata.json'), 'compiler metadata'), 'utf8'),
  );
  await writeFile(
    join(out, 'rules/counts.json'),
    `${JSON.stringify(target.counts(metadata), null, 2)}\n`,
  );

  // 6. Attribution must travel with the lists it covers (§20).
  await cp(join(dist, 'ATTRIBUTION.txt'), join(out, 'ATTRIBUTION.txt'));

  const size = await directorySize(out);
  console.log(`${name.padEnd(9)} -> ${out}  (${(size / 1024 / 1024).toFixed(1)} MiB)`);
}

async function directorySize(dir) {
  let total = 0;
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) total += await directorySize(path);
    else total += (await readFile(path)).byteLength;
  }
  return total;
}

const requested = process.argv.slice(2);
const targets = requested.length > 0 ? requested : Object.keys(TARGETS);
for (const target of targets) {
  await buildTarget(target);
}
