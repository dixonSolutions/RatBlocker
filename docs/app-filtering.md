# Filtering browsers and native applications

RatBlocker enforces only decisions that each platform can make from data it
actually observes. It does not label content as suspicious with an opaque
heuristic.

## Protection matrix

| Capability | Browser pages and installed web apps | Native applications |
| --- | --- | --- |
| Known ad/tracker host blocking | Full URL and request context | Not covered |
| Internal/first-party ad URL matching | Yes, including path and popup context | Not covered |
| Popup blocking | Yes, for destinations matched by `$popup` rules | Not covered |
| Ad element hiding | Yes, with supported EasyList cosmetic selectors | Not covered |
| In-video ad decisions (YouTube) | Yes, by pruning the player response | Not covered |

Native desktop applications are outside RatBlocker's scope. The Linux DNS
daemon that once covered them has been removed: it could only ever match a
bare hostname, which is not enough to distinguish an ad from content on any
service that serves both from one origin. A system-wide Android service is
planned and will face the same hostname-only limit.

Browser extensions also run in browser-installed web apps when the browser
allows extensions on their pages.

## Popup and internal-link decisions

`$popup` is a navigation constraint, not an alias for every document request.
The browser adapters mark a newly created tab or window as popup context and
evaluate its complete destination URL together with the opener URL. This lets
rules detect a first-party URL such as `/sponsored/out` without blocking an
ordinary visit to that URL in the current tab.

Chromium's declarative network API does not expose popup context. Popup rules
therefore remain in the small WebAssembly database and are enforced by the
extension background worker. The same core decision is used on Firefox.

"Suspicious" means that a trusted or user-authored filter rule matched. Closing
unknown popups merely because they opened a new tab would create unacceptable
false positives for sign-in, payment, and document workflows.

## Native-application boundary

A hostname is not enough. A DNS-level filter sees a name, a query type and a
local source; it cannot see an HTTPS URL, the link that initiated a request,
whether an application opened a popup, or the application's widget tree. That
is sufficient to block a dedicated ad or tracker host, and insufficient for any
service that serves ads and content from the same origin — which is now the
common case, not the exception.

Removing arbitrary UI elements would require accessibility automation,
application-specific plugins, or compositor injection. Those mechanisms can
read or manipulate unrelated sensitive UI and can break application behavior,
so RatBlocker does not use them. A future native integration must use an
explicit, application-owned extension API and fail closed at its network
boundary rather than simulating clicks or hiding windows globally.
