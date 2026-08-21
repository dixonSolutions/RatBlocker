# Filtering browsers and native applications

RatBlocker enforces only decisions that each platform can make from data it
actually observes. It does not label content as suspicious with an opaque
heuristic.

## Protection matrix

| Capability | Browser pages and installed web apps | Native applications |
| --- | --- | --- |
| Known ad/tracker host blocking | Full URL and request context | Hostname through the system DNS proxy |
| Internal/first-party ad URL matching | Yes, including path and popup context | Only when the hostname itself is blocked |
| Popup blocking | Yes, for destinations matched by `$popup` rules | No: DNS cannot tell why an app resolved a host |
| Ad element hiding | Yes, with supported EasyList cosmetic selectors | No general, safe platform API |

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

The Linux daemon sees DNS questions: a hostname, query type, and local network
source. It cannot see an HTTPS URL, the link that initiated a request, whether
the application opened a popup, or the application's widget tree. It can still
block a known ad or tracker hostname before any connection is made, which
covers all applications using the system resolver.

Removing arbitrary UI elements would require accessibility automation,
application-specific plugins, or compositor injection. Those mechanisms can
read or manipulate unrelated sensitive UI and can break application behavior,
so RatBlocker does not use them. A future native integration must use an
explicit, application-owned extension API and fail closed at its network
boundary rather than simulating clicks or hiding windows globally.
