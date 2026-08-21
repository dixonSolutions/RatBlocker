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
