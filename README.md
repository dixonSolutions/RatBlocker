![RatBlocker](design/banner.png)

# RatBlocker

RatBlocker is an ad and tracker blocker that runs entirely on your own machine.
It has no account, no cloud service, no telemetry, and it never intercepts TLS:
filtering happens in a shared Rust engine compiled to WebAssembly for the
browser extensions, so the same rules and the same decisions apply everywhere
it runs. Filter lists are compiled ahead of time into a binary database that
ships with the build, which means a fresh install blocks from the first request
without contacting anything.

RatBlocker is a browser extension, for Chromium-based browsers (Chromium,
Google Chrome) and Gecko-based browsers (Firefox and its forks). It does not
filter outside the browser on any platform; see [`docs/scope.md`](docs/scope.md)
for what that covers and why the Linux system daemon was removed.

## What is here

- **`core/`** — the filtering engine (`ratblocker-core`), `#![forbid(unsafe_code)]`.
  Parses EasyList/Adblock Plus syntax, hosts files and bare domain lists;
  matches requests through a hostname/token index; supports exception rules,
  `$important`, `$badfilter`, `$domain`, `$denyallow`, `$removeparam` and
  `$redirect` to bundled no-op resources. Unsupported options are rejected and
  reported rather than silently ignored. Cosmetic filtering covers `##` and
  `#@#`; scriptlet injection (`+js(...)`) is deliberately not supported.
- **`filter-compiler/`** — `ratblocker-compile`, which turns filter lists into
  the binary rule database, the Chromium `declarativeNetRequest` rulesets, the
  no-op redirect resources and `ATTRIBUTION.txt`.
- **`extensions/`** — Chromium (MV3, Chrome 121+) and Firefox (MV2, Firefox
  128+) extensions in TypeScript, sharing the popup and settings UI. The engine
  is loaded as WebAssembly through a hand-written ABI in
  `extensions/shared/wasm` (no wasm-bindgen).
- **Popup and internal-link filtering** — browser-created tabs and windows are
  evaluated with their opener and full destination URL, so `$popup` rules can
  match first-party ad paths without blocking ordinary same-tab navigation.
  See [`docs/app-filtering.md`](docs/app-filtering.md) for what each layer can
  and cannot see.
- **`filter-lists/bundled/`** — the EasyList and EasyPrivacy snapshots that the
  compiler consumes.
- **`tests/`** — security and performance suites, plus Chromium (CDP) and
  Firefox (Marionette) browser integration harnesses.
- **`design/`** — the icon and banner sources. See `design/README.md`.
- **`site/`** — the project site (Angular 22 + Optimus UI), prerendered and
  published to GitHub Pages. Its facts and figures are generated from the filter
  compiler's output rather than maintained by hand. See `site/README.md`.

Local statistics exist but are off by default, and never record URLs.

## Status

Early. The version across the workspace is `0.1.0` and nothing has been
released. The engine, the compiler and both browser extensions are implemented
and covered by tests, but the project has rough edges you should know about
before relying on it:

- The comprehensive `docs/architecture.md` cited by source comments has not
  been reconstructed yet; focused platform behavior is documented in `docs/`.
- **Android is not implemented.** A system-wide Android service is the next
  planned platform; no such code exists in this repository yet.
- **There is no system-wide filtering on desktop.** Filtering applies inside
  supported browsers only. Native desktop applications are not covered.
- YouTube in-video ads are handled by pruning ad decisions out of the player
  response in the extension (`extensions/shared/src/streaming-ads.ts`). This is
  inherently a moving target: when YouTube renames those fields it stops
  working until the extension is updated.
- Distribution URLs in the packaging artifacts are placeholders. Override them
  with `RATBLOCKER_UPDATE_BASE` when packaging.
- Filter-update signing is implemented (ed25519, detached) but no signing key
  is published, so only the explicit-trust path guards third-party lists.

## Building

Requires Rust 1.82 or newer and a recent Node.js (the build scripts are ESM and
use `import.meta.dirname`, so Node 20.11+).

Build the Rust workspace — the engine and the filter compiler:

```sh
cargo build --release
```

Compile the bundled filter lists into `dist/`. This produces `rules.rbdb`, the
Chromium rulesets, the redirect resources, `metadata.json` and
`ATTRIBUTION.txt`:

```sh
./target/release/ratblocker-compile build \
    --list easylist=filter-lists/bundled/easylist.txt \
    --list easyprivacy=filter-lists/bundled/easyprivacy.txt \
    --out dist
```

Add `--report-rejects` to also write `dist/rejected.txt` listing every rule the
parser refused and why.

The extension build consumes the compiled database and a WebAssembly build of
the engine, so both must exist first:

```sh
rustup target add wasm32-unknown-unknown
cargo build --release -p ratblocker-wasm --target wasm32-unknown-unknown

cd extensions && npm install && node build.mjs
```

`node build.mjs` builds both targets into `extensions/chromium/build/` and
`extensions/firefox/build/`; pass `chromium` or `firefox` to build just one.
Either directory can be loaded as an unpacked extension for development.

## Installing

### Browsers, without a store

```sh
cd extensions && node package.mjs
```

For **Chromium**, this produces a signed CRX, an update manifest and the Linux
external-extension descriptor. The extension id is derived from
`chromium-signing-key.pem`, which is generated on first run and is gitignored —
keep it, because losing it changes the id and orphans every existing install.
Installing without the Web Store:

```sh
sudo install -Dm644 dist/ratblocker-chromium.crx /usr/share/ratblocker/ratblocker-chromium.crx

# Chromium
sudo install -Dm644 dist/<extension-id>.json /usr/share/chromium/extensions/

# Google Chrome reads the same descriptor from its own directory
sudo install -Dm644 dist/<extension-id>.json /opt/google/chrome/extensions/
```

The same CRX and descriptor serve both browsers; only the directory differs.

For genuinely remote hosting, publish `ratblocker-chromium.crx` and
`chromium-update.xml` and deploy `dist/chromium-policy.json` to
`/etc/chromium/policies/managed/` (Chromium) or
`/etc/opt/chrome/policies/managed/` (Google Chrome).

For **Firefox**, this produces an XPI. Release Firefox refuses to permanently
install an unsigned extension and no preference or policy overrides that, so
there are two routes:

- `node sign-firefox.mjs` submits the XPI to addons.mozilla.org for *unlisted*
  signing — no review queue, nothing listed publicly, and you host the signed
  file and its updates yourself. It needs an AMO API key.
- Gecko forks built without `MOZ_REQUIRE_SIGNING` — Zen, LibreWolf, Waterfox,
  Mullvad Browser — and Mozilla's own Developer Edition, Nightly, ESR and
  unbranded builds install the unsigned XPI directly once
  `xpinstall.signatures.required` is `false`.

`node tests/browser/gecko-signing.mjs <browser-binary> <xpi>` answers which
category a given build falls into, by trying a permanent unsigned install.

## Licensing

RatBlocker's own code is licensed **GPL-3.0-or-later**.

The filter lists bundled under `filter-lists/bundled/` are not ours. EasyList
and EasyPrivacy are published by the EasyList project under
**GPL-3.0-or-later** and **CC-BY-SA-3.0**; see
<https://easylist.to/pages/licence.html>.

Every build records provenance for the lists it compiled in
`dist/ATTRIBUTION.txt` — the name, version, upstream source, home page,
licence and rule count of each list. That file is copied into each extension
build, so a shipped artifact always carries the attribution for the data
inside it.

`core/data/public_suffix_list.txt` is the Mozilla Public Suffix List, used
under **MPL-2.0**; its licence is kept next to it in
`core/data/public_suffix_list.LICENSE`.
