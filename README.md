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

### Guided setup

The shortest route on any operating system. Bash for Linux and macOS,
PowerShell for Windows; neither needs anything else installed:

```sh
cd extensions
./setup.sh                 # Linux, macOS
```

```powershell
cd extensions
.\setup.ps1                # Windows
```

It lists what it found and waits for a selection:

```
Found:

  1 ✓ Zen 1.21.15b                   gecko     flatpak
        app.zen_browser.zen
        2 profiles
  2 ✗ Firefox 153.0.4                gecko     system
        a Mozilla release build enforces signing and would reject an unsigned XPI
  3 ! Chromium 151.0.7922.108        chromium  system
        /usr/share/chromium/extensions

Select browsers: numbers (1 3), a range (1-3), "a" for all, "q" to quit.
```

Nothing is touched without being chosen. `--dry-run` shows the plan and changes
nothing, `--update` installs only where what is there is older, `--uninstall`
reverses it, `--list` and `--json` just report, and `--all --yes` skips the
questions. `--xpi <path|url>` installs a particular build — a signed one from
Mozilla, for instance, rather than a local one.

**Nothing in that list is hardcoded, and no browser is named anywhere in either
script.** Browsers are found by asking the machine what is installed, and each
one is identified by the engine it actually ships — `libxul.so` or `xul.dll`
for Gecko, `resources.pak` beside `icudtl.dat` for Chromium — so a fork
released after this was written is found on its own terms. Everything else is
read out of the installation too:

- the name and version from `application.ini`, or from the desktop entry that
  claims to handle `http`, or from the package manager that owns the file;
- the profile directory derived the way Gecko itself derives it, from
  `[App] Profile` or `Vendor`/`Name`, and then confirmed against the
  `compatibility.ini` Gecko leaves in every profile naming the installation it
  last ran from — which is how a profile that has moved is still matched to its
  browser, and how a profile whose browser is gone is still recognised;
- a Chromium build's external-extension directory out of the strings in its own
  binary, where the compiler left it.

Flatpak and snap are asked directly what they have and where they put it, and
the sandboxed home each one hands the browser is scanned separately — a flatpak
is a different browser from a native install of the same name, because its
profiles are somewhere the native install will never look.

Two rules hold throughout both scripts, and both matter:

- **Nothing found is ever executed.** Asking a binary its version is how you
  open half a dozen windows on someone's desktop, because plenty of things that
  embed a browser engine are not browsers. Versions come from files and from
  the package manager.
- **Embedding an engine is not being a browser.** Every Electron application
  ships Chromium's `.pak` files and Thunderbird ships the same libxul. Both are
  recognised — an application carries its own code in `resources/app.asar`, and
  a Gecko browser ships `browser/omni.ja` where a mail client does not — and
  both are reported as skipped rather than silently offered or silently
  dropped.

`extensions/tests/setup-script.test.mjs` holds the two scripts to one
specification, building synthetic browsers in a temporary directory and running
the real scripts against them. It runs on Linux, macOS and Windows in CI, so
the two implementations cannot drift apart unnoticed.

### Browsers, without a store

To drive it from a script, or to package for distribution, build first:

```sh
cd extensions && node build.mjs
RATBLOCKER_UPDATE_BASE=https://your.host/ratblocker node package.mjs
node install.mjs --dry-run     # what it would do, changing nothing
node install.mjs               # do it
```

`RATBLOCKER_UPDATE_BASE` is where the packaged extensions will look for
updates. It is baked into the artifacts at packaging time, so set it before
packaging anything you intend to distribute; see *Staying up to date* below.
Left unset it falls back to a placeholder host, which is fine for local use and
useless for distribution.

`install.mjs` finds the supported browsers on the machine and uses the right
mechanism for each. `--uninstall` reverses it; naming browsers
(`node install.mjs zen chromium`) restricts it.

`--update` installs only where what is present is older, and never downgrades.
That is the whole update mechanism for every target that does not need a
server: Gecko reads the version out of the profile at startup and Chromium
rescans its external descriptors at every start, so replacing the file *is* the
update. Run it from a login item or a timer and installs stay current with no
hosting at all. See *How updates actually reach an install* in
[`docs/scope.md`](docs/scope.md) for which targets that covers.

| Browser | Mechanism | Root | Unsigned XPI |
| --- | --- | --- | --- |
| Chromium | CRX + external-extension descriptor | yes | n/a |
| Google Chrome | CRX + external-extension descriptor | yes | n/a |
| Zen, LibreWolf, Waterfox | XPI into each profile | no | accepted |
| Firefox (release) | XPI into each profile | no | **refused** |

The two families differ in ways that are worth knowing before something
surprises you.

**Chromium-based browsers have no per-user external-extension directory on
Linux.** A store-free install that survives restarts has to write to a system
path, so it needs root. The script performs the work when run as root and
otherwise prints the exact commands, which look like this:

```sh
# Chromium
sudo sh -c 'install -Dm644 dist/ratblocker-chromium.crx /usr/share/ratblocker/ratblocker-chromium.crx \
  && install -Dm644 dist/<extension-id>.json /usr/share/chromium/extensions/<extension-id>.json'

# Google Chrome — same two files, different directory
sudo sh -c 'install -Dm644 dist/ratblocker-chromium.crx /usr/share/ratblocker/ratblocker-chromium.crx \
  && install -Dm644 dist/<extension-id>.json /opt/google/chrome/extensions/<extension-id>.json'
```

`<extension-id>` is printed by `package.mjs` and is derived from
`chromium-signing-key.pem`. Restart the browser afterwards.

**Gecko browsers install per profile and need no privileges**, but whether they
accept an *unsigned* XPI depends on how the binary was compiled.
`xpinstall.signatures.required` is only honoured where `MOZ_REQUIRE_SIGNING`
was off. Release Firefox enforces it and rejects the XPI outright, so
`install.mjs` refuses rather than leaving a silently disabled add-on behind.
Zen, LibreWolf, Waterfox, Developer Edition, Nightly, ESR and Mozilla's
unbranded builds accept it.

Flatpak and snap installs are treated as separate browsers, because they are: a
sandboxed package keeps its profiles under `~/.var/app/` or `~/snap/`, and
installing there would not affect a native install of the same browser.

The setup scripts work this out per build instead of consulting a table: a
default preference that turns the check off settles it, then the update channel
the build ships, then who built it. To settle it by experiment rather than by
inference — the only way to be certain — ask the build directly:

```sh
node tests/browser/gecko-signing.mjs <browser-binary> dist/ratblocker-firefox-0.1.0.xpi
```

For release Firefox there are three routes:

- **Listed.** `node sign-firefox.mjs --channel listed` submits the XPI to the
  public Firefox add-on catalogue. Mozilla signs it, hosts it, serves its
  updates, and gives it a page with an install button — which is what the *Get
  it as a Mozilla Add-on* button on the site links to. It goes through a review
  queue, and the listing is public.
- **Unlisted.** `node sign-firefox.mjs` submits it for unlisted signing — no
  review queue, nothing public, and you host the signed file and its updates
  yourself. Once signed, both setup scripts and `install.mjs` install it like
  any other.
- Use a build that does not enforce signing, from the list above.

Both need an AMO API key. Generate one at
<https://addons.mozilla.org/developers/addon/api/key/>, then put it where the
workflow can reach it:

```sh
gh secret set AMO_JWT_ISSUER
gh secret set AMO_JWT_SECRET
```

Locally, the same two values go in the environment or in `.amo-credentials`,
which is gitignored. Never commit them, and treat a key that has been pasted
anywhere as spent — revoke and regenerate it on the same page.

### Publishing

`.github/workflows/publish-amo.yml` builds the extension, validates it, and
submits it. It runs when a GitHub release is published, or on demand from the
Actions tab, where the channel is a dropdown and `dry_run` builds and validates
without submitting anything. Publishing is never triggered by an ordinary push:
a listed submission is public and cannot be taken back.

Once a listed submission is accepted, the workflow records the slug AMO
assigned into `site/src/app/data/project.ts`, which is what turns the install
button on. Until then the site says the add-on is not in the catalogue yet
rather than offering a button that leads nowhere.

## Staying up to date

An extension installed outside a store still updates through the browser's own
machinery, as long as the packaged manifest says where to look. `package.mjs`
writes that address into both builds and generates the manifests they poll:

| File | Polled by | Points at |
| --- | --- | --- |
| `dist/chromium-update.xml` | Chromium, Chrome | `ratblocker-chromium.crx` |
| `dist/firefox-updates.json` | Firefox and forks | the versioned `.xpi` |

Serve `dist/` over HTTPS at whatever you set `RATBLOCKER_UPDATE_BASE` to, and
publish a new build by dropping the new CRX or XPI beside an updated manifest.
The browser verifies the signature and installs it; RatBlocker never downloads
or applies an update itself.

Both engines poll on their own schedule — Chromium roughly every five hours,
Gecko about once a day — so a browser that is opened and closed inside that
window can go a long time without checking. The extension therefore asks for a
check at startup as well, through `runtime.requestUpdateCheck`, rate-limited to
once every six hours. There is also a manual `checkForUpdates` message.

Two things to know:

- **`update_link` must be HTTPS.** Firefox refuses to fetch an update over
  plain HTTP no matter how the file is signed.
- **Release Firefox will not update to an unsigned XPI**, for the same reason
  it will not install one. Self-hosted Gecko updates need AMO unlisted signing,
  or a build that does not enforce signing.

### Hosting the downloads

Windows and macOS need somewhere to fetch from, and the repository already
publishes to GitHub Pages. The Pages workflow builds and packages the
extensions and puts them under `/downloads/`, so the update URL baked into
every artefact is:

```
https://<owner>.github.io/<repo>/downloads/
```

Add the CRX signing key as a repository secret named `CHROMIUM_SIGNING_KEY`,
base64-encoded:

```sh
base64 -w0 chromium-signing-key.pem
```

The key stays out of the repository, and the extension id is derived from it —
a different key means a different id, which orphans every existing install.
Without the secret the workflow still publishes the site and the XPI, and warns
rather than failing.

For an air-gapped network, point `RATBLOCKER_UPDATE_BASE` at any HTTP server
that can reach the browsers, including `http://localhost:8080`. Chrome accepts
plain HTTP for update manifests; Firefox requires HTTPS, but Gecko installs are
updated by replacing the file in the profile and need no server at all.

### Serving the downloads locally

When publishing is not an option, `node serve.mjs` serves `dist/` over plain
HTTP, which Chrome accepts for update manifests including on `localhost`:

```sh
RATBLOCKER_UPDATE_BASE=http://127.0.0.1:8080 node package.mjs
node serve.mjs --port 8080
```

It binds loopback unless told otherwise. `RATBLOCKER_UPDATE_BASE` must match
where the files will really be served from: the address is written into the
artefacts when they are packaged, not when they are served.

### Chromium and Chrome, off-store, on every platform

This works on Chrome as well as Chromium, and on all three desktop platforms —
but not by the same mechanism, because Chrome does not offer one.

**On Linux**, an external-extension descriptor installs a local CRX directly,
and the browser then updates it from the `update_url` baked into that CRX.
Nothing needs hosting for the install itself.

**On Windows and macOS, Chrome refuses to install an external extension that
is not hosted in the Chrome Web Store.** The descriptor route does not exist
there. Enterprise policy is the supported path, and it works for off-store
extensions — but policy tells the browser *where to fetch from* rather than
handing it a file, so on those platforms **the CRX must be reachable over HTTPS
at `RATBLOCKER_UPDATE_BASE`**. A purely local install is not possible.

`package.mjs` writes the policy artefact for each platform into `dist/policy/`:

| Platform | Artefact | Applied by |
| --- | --- | --- |
| Linux | `linux-policy.json` | copy to `/etc/opt/chrome/policies/managed/` or `/etc/chromium/policies/managed/` |
| Windows | `windows-chrome.reg`, `windows-chromium.reg` | `reg import` from an elevated prompt |
| macOS | `macos-com.google.Chrome.plist`, `macos-org.chromium.Chromium.plist` | MDM, or copy to `/Library/Managed Preferences/` |

`install.mjs` detects the platform and prints the right commands. To see what
another platform would be told, without changing anything:

```sh
node install.mjs --dry-run --platform darwin
node install.mjs --dry-run --platform win32
```

On a managed Mac or a domain-joined Windows machine, deploy the plist or the
registry values through MDM or Group Policy rather than writing them by hand;
the generated files describe exactly the same settings.

All three name the same extension id and the same `update_url`, so whichever
route installs it, updates arrive the same way.

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
