# RatBlocker site

The project site, published to GitHub Pages. Angular 22 with
[Optimus UI](https://github.com/openng-org/optimus-ui) 2.0.1, fully prerendered
— GitHub Pages serves static files, so there is nothing at request time to
render on.

## Where the numbers come from

Rule counts, database sizes, artifact sizes, list versions and checksums are
**generated**, not typed in. `tools/generate-facts.mjs` reads
`dist/metadata.json` — the filter compiler's own output — and writes
`src/app/data/build-facts.ts`. It runs automatically before every build.

That means the site cannot quietly drift from what the project actually
produces. It also means you must compile the filter lists before building the
site, and the generator fails loudly rather than emitting placeholders if you
have not:

```sh
cargo build --release -p ratblocker-filter-compiler
./target/release/ratblocker-compile build \
    --list easylist=filter-lists/bundled/easylist.txt \
    --list easyprivacy=filter-lists/bundled/easyprivacy.txt \
    --out dist
```

`tools/sync-assets.mjs` copies `design/` into `public/design/` for the same
reason: the icon lives at the repository root because the extensions and the
README use it too, and the Angular builder refuses asset paths outside the
workspace. Both generated paths are gitignored.

## Developing

```sh
npm install
npm start          # http://localhost:4200
npm run build      # prerenders into dist/site/browser
```

The build defaults to a base href of `/RatBlocker/`. The Pages workflow
overrides it from the repository name, so a fork or a rename deploys correctly
without editing `angular.json`:

```sh
npm run build -- --base-href /your-repo-name/
```

## Structure

- `src/app/app.ts` — shell: header, navigation, theme toggle, footer
- `src/app/pages/home` — what it is, and what it refuses to do
- `src/app/pages/how-it-works` — the decision pipeline, per-platform mechanisms,
  the Manifest V3 rule-cap problem, and the documented limitations
- `src/app/pages/install` — per-platform instructions, including the Firefox
  signing situation
- `src/app/pages/releases` — artifacts, sizes and checksums from the last build
- `src/app/data/project.ts` — prose and links; everything countable is generated

Theming is the Optimus Aura preset with its primary tokens repointed at
RatBlocker's accent, so the site, the extension popup and the toolbar icon read
as one thing. Dark mode follows the system preference and can be overridden by
the header toggle.
