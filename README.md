![RatBlocker](design/banner.png)

# RatBlocker

RatBlocker is an ad and tracker blocker that runs entirely on your own machine.
It has no account, no cloud service, no telemetry, and it never intercepts TLS:
filtering happens in a shared Rust engine that is compiled once into a native
Linux daemon and once into WebAssembly for the browser extensions, so the same
rules and the same decisions apply everywhere. Filter lists are compiled ahead
of time into a binary database that ships with the build, which means a fresh
install blocks from the first request without contacting anything.

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
- **`linux/`** — `ratblockerd`, a D-Bus service that owns the engine, plus a
  caching DNS proxy (including DNS-over-TLS upstreams), a filter updater that
  verifies ed25519 detached signatures and rolls back to the last known good
  database, a minimal privileged helper for pointing `systemd-resolved` at the
  proxy, and `ratblocker`, an unprivileged command-line client. Mutating D-Bus
  calls are authorised through Polkit.
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
released. The engine, the compiler, the Linux daemon and both browser
extensions are implemented and covered by tests, but the project has rough
edges you should know about before relying on it:

- `docs/` and `security/` are empty, even though source comments cite
  `docs/architecture.md` and its section numbers throughout.
- Android and GNOME frontends are described in the architecture but no such
  code exists in this repository yet.
- The GTK settings application does not exist yet; the daemon is driven by the
  `ratblocker` command-line client.
- Distribution URLs in the packaging artifacts are placeholders. Override them
  with `RATBLOCKER_UPDATE_BASE` when packaging.
- Filter-update signing is implemented (ed25519, detached) but no signing key
  is published, so only the explicit-trust path guards third-party lists.

## Building

Requires Rust 1.82 or newer and a recent Node.js (the build scripts are ESM and
use `import.meta.dirname`, so Node 20.11+).

Build the Rust workspace — engine, filter compiler, daemon and CLI:

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

### Linux daemon

```sh
sudo bash linux/packaging/install.sh
sudo systemctl enable --now ratblockerd
ratblocker status
```

The daemon runs under its own unprivileged account with
`CAP_NET_BIND_SERVICE` and nothing else, and answers DNS on `127.0.0.2:53`.
Installing it does **not** redirect the system's DNS; that is a separate unit,
so the change only happens when you ask for it and stopping the unit puts your
resolver configuration back:

```sh
sudo systemctl enable --now ratblocker-dns
```

`sudo bash linux/packaging/uninstall.sh` reverses everything and restores DNS
first. Configuration and state are left behind deliberately.

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
sudo install -Dm644 dist/<extension-id>.json /usr/share/chromium/extensions/
```

For genuinely remote hosting, publish `ratblocker-chromium.crx` and
`chromium-update.xml` and deploy `dist/chromium-policy.json` to
`/etc/chromium/policies/managed/`.

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
build and installed alongside the daemon, so a shipped artifact always carries
the attribution for the data inside it.

`core/data/public_suffix_list.txt` is the Mozilla Public Suffix List, used
under **MPL-2.0**; its licence is kept next to it in
`core/data/public_suffix_list.LICENSE`.
