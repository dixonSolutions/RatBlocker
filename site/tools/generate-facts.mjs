/**
 * Generate `src/app/data/build-facts.ts` from the filter compiler's own output.
 *
 * The numbers on the site — rule counts, database sizes, list versions — are
 * the ones a build actually produced, not figures typed into a template that
 * quietly rot. If `dist/metadata.json` is missing the generator fails loudly
 * rather than emitting plausible-looking placeholders.
 */

import { readFile, writeFile, stat } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, '../..');
const dist = join(repo, 'dist');

async function sizeOf(path) {
  try {
    return (await stat(join(dist, path))).size;
  } catch {
    return 0;
  }
}

const metadata = JSON.parse(
  await readFile(join(dist, 'metadata.json'), 'utf8').catch(() => {
    throw new Error(
      'dist/metadata.json not found. Run the filter compiler first:\n' +
        '  ./target/release/ratblocker-compile build --list easylist=... --out dist',
    );
  }),
);

const facts = {
  generatedFrom: 'dist/metadata.json',
  rules: metadata.rules,
  rejected: metadata.rejected,
  chromium: {
    rulesEmitted: metadata.chromium.kept,
    collapsedDomains: metadata.chromium.collapsed_domains,
    collapsedIntoRules: metadata.chromium.collapsed_into_rules,
    candidates: metadata.chromium.candidates,
    droppedOverBudget: metadata.chromium.dropped_over_budget,
    maxStaticRules: metadata.chromium.limits.max_static_rules,
  },
  database: {
    bytes: metadata.database.bytes,
    sha256: metadata.database.sha256,
    cosmeticBytes: metadata.chromium_cosmetic_database?.bytes ?? 0,
  },
  artifacts: {
    crxBytes: await sizeOf('ratblocker-chromium.crx'),
    xpiBytes: await sizeOf('ratblocker-firefox-0.1.0.xpi'),
  },
  sources: metadata.sources.map((s) => ({
    id: s.id,
    title: s.title ?? s.id,
    version: s.version ?? null,
    license: s.license ?? null,
    homepage: s.homepage ?? null,
    ruleCount: s.rule_count,
    checksum: s.checksum ?? null,
  })),
};

const banner = `// GENERATED FILE — do not edit.
// Produced by site/tools/generate-facts.mjs from ${facts.generatedFrom}.
// Run \`npm run facts\` after recompiling the filter lists.
`;

await writeFile(
  join(here, '../src/app/data/build-facts.ts'),
  `${banner}\nexport const BUILD_FACTS = ${JSON.stringify(facts, null, 2)} as const;\n`,
);

console.log(
  `wrote src/app/data/build-facts.ts (${facts.rules.network} network rules, ` +
    `${facts.chromium.rulesEmitted} Chromium rules)`,
);
