# What RatBlocker covers, and what it deliberately does not

RatBlocker is a **browser extension**, for Gecko-based and Chromium-based
browsers. It is not a system-wide filter on any platform. This document records
why, because the question recurs and the answer is a trade rather than an
oversight.

## Supported

| Browser family | Targets | Manifest |
| --- | --- | --- |
| Chromium-based | Chromium, Google Chrome (121+) | MV3 |
| Gecko-based | Firefox 128+, and forks built without `MOZ_REQUIRE_SIGNING` | MV2 |

Firefox for Android is declared in the manifest (`gecko_android`) but has not
been tested on a device. Treat it as unverified until it has been.

### Installing without a store

`extensions/install.mjs` handles all of them; see the README. Two constraints
shape it, both verified on this project's own artifacts rather than assumed:

- Release Firefox refuses an unsigned XPI outright
  (`ERROR_SIGNEDSTATE_REQUIRED`), so the installer refuses too rather than
  leaving a disabled add-on in the profile. Zen accepts one permanently.
  `tests/browser/gecko-signing.mjs` answers this for any build.
- Chromium-based browsers have no per-user external-extension directory on
  Linux, so a persistent store-free install requires root. That is a property
  of the browsers, not a choice this project made.

## Removed: the Linux system daemon

A DNS proxy, D-Bus service, Polkit policy and CLI once shipped under `linux/`.
It worked — it blocked around a third of all DNS queries in ordinary browsing —
and it was removed anyway, because a hostname is the wrong unit of decision.

DNS filtering answers one question: *should this name resolve?* That is enough
for a host that exists only to serve ads, and useless for any service that
serves ads and content from the same origin. YouTube is the clearest case: ad
segments and video segments both come from `*.googlevideo.com`, chosen by a
same-origin response. Blocking the hostname does not remove the ad, it removes
the video.

Since that pattern is now the norm rather than the exception, a hostname-only
layer promises more than it delivers. Better to cover fewer places honestly.

## Not planned: system-wide Android

A no-root Android service means `VpnService` with a local TUN filtering DNS —
the same architecture, and the same limit. It would not block ads in the
YouTube app, and on Android there is no extension to fall back on.

Root does not change this. Root's only relevant power is installing a CA into
the system trust store, which buys HTTPS interception. That would mean
RatBlocker decrypting and rewriting every TLS stream on the device, abandoning
the promise it leads with, in exchange for a fragile foothold that ReVanced and
alternative clients already hold more cleanly. Not worth it.

## Not planned: scriptlets from filter lists

`+js(...)` rules are rejected by the parser and always will be. Executing code
that arrives through the filter-list update channel makes every list author a
code-execution vector.

This is not the same as refusing the *technique*. See below.

## In-video ad blocking, without remote code

`extensions/shared/src/streaming-ads.ts` prunes ad decisions
(`adPlacements`, `adSlots`, `playerAds`, `adBreakHeartbeatParams`,
`playerConfig.adPlacementConfig`) out of YouTube player responses before the
player reads them. Same effect as uBlock Origin's `json-prune` scriptlet.

The difference is provenance. This is first-party source in this repository,
reviewed and shipped like any other module. No filter list can introduce it,
change it, or add another one. The update-channel risk that `+js(...)` carries
is absent.

It runs in the page's MAIN world, because it must replace `JSON.parse` and
`Response.prototype.json` as the page sees them. It starts **inert** and prunes
only after the isolated content script confirms filtering applies, so a
disabled extension, an active pause, or an allowlisted `youtube.com` all result
in no pruning. The activation channel honours only *enable*: a page that forged
the message could at worst switch blocking on for itself.

### This will break, periodically

YouTube renames these fields from time to time. When it does, pruning silently
stops working until the extension is updated — and because the logic is bundled
rather than list-driven, that means a release, not a list refresh. That is the
cost of not executing remote code, and it is accepted deliberately.

`extensions/tests/streaming-ads.test.mjs` guards our own logic. It cannot
detect a rename upstream. The check for that is manual: load a monetized video
in a fresh profile and confirm the real video starts immediately, rather than a
15–30 second pre-roll.


## How updates actually reach an install

Four cases, and they do not share one mechanism. Where this says *verified*, it
was tested against the real browser rather than read from documentation.

| Target | Needs a server? | Mechanism |
| --- | --- | --- |
| Zen, LibreWolf, Waterfox, Firefox ESR/Dev/Nightly | No | replace `<profile>/extensions/<id>.xpi` — *verified: 0.1.0 to 0.1.1* |
| Firefox release | No, but needs AMO signing | same, once the XPI is signed |
| Chromium and Chrome on **Linux** | No | replace the CRX and bump `external_version`; Chrome rescans external descriptors at every start |
| Chrome and Chromium on **Windows or macOS** | **Yes** | enterprise policy naming an `update_url` |

`extensions/install.mjs` is the update mechanism for the first three: re-running
it after a version bump is an update. Nothing else is required.

The fourth is different because of a decision Chrome made, not one this project
made. Local-CRX external installs were removed from Windows in Chrome 33 and
from macOS in Chrome 44, so there is no local install path there and therefore
no local update path. Two routes that look like workarounds are not:
`--load-extension` was removed from branded Chrome in 137 (and the override
flag in 142), and developer-mode unpacked extensions are switched off again by
Chrome updates.

That leaves enterprise policy, which names a URL rather than a file. The
repository's GitHub Pages deployment serves it, so the requirement costs
nothing; `http://localhost` also works, since Chrome accepts plain HTTP for
update manifests.

### The two cases that need more than file replacement

**Chromium and Chrome on Linux need root — but only once.** The
external-extension directory is system-owned, so the initial install is
privileged. Updates are not. Once installed, the extension is an ordinary
installed extension, and the browser updates it from the `update_url` inside
the CRX into the user's own profile with no privileges at all. The external
descriptor only bootstraps; Chrome will not downgrade a profile copy that is
newer than `external_version`.

So `install.mjs --update` reports an installed Chromium as up to date and does
nothing, rather than asking for root it does not need. Root is only requested
when the extension is genuinely absent.

**Windows and macOS need a URL**, because policy is the only off-store route
and policy names an address rather than a file. Two ways to provide one:

- **Publish it.** The Pages workflow already does, at
  `https://<owner>.github.io/<repo>/downloads/`. Nothing further is required.
- **Serve it locally.** `node serve.mjs` serves `dist/` over plain HTTP, which
  Chrome accepts for update manifests, including on `localhost`. Package with a
  matching base so the URLs inside the artefacts agree:

  ```sh
  RATBLOCKER_UPDATE_BASE=http://127.0.0.1:8080 node package.mjs
  node serve.mjs --port 8080
  ```

  It binds loopback by default, since it serves installable extension packages.
  Run it as a login item on an isolated network and Chrome will poll it at
  startup like any other update host.

Gecko never needs either: `install.mjs --update` replaces the file in the
profile and the browser picks it up at the next start.

### One packaging rule to remember

The update address is baked into the artefacts at packaging time, so
`RATBLOCKER_UPDATE_BASE` must match wherever they will actually be served from.
Packaging for Pages and then serving from localhost produces artefacts that
point at Pages, and vice versa.
