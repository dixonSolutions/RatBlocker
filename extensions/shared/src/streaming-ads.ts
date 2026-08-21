/**
 * In-page ad-decision pruning for streaming sites, YouTube in particular.
 *
 * Why this exists at all, given that `+js(...)` scriptlets are rejected by the
 * parser: YouTube serves its ads from the same hostnames as the video itself
 * (`*.googlevideo.com`) and decides what to play from a same-origin InnerTube
 * response. There is no request to block and no element to hide — by the time
 * anything is on the wire, the ad and the content are indistinguishable.
 *
 * The only place the two are still separable is the player response, which
 * names its ad breaks in `adPlacements` / `adSlots` / `playerAds`. Removing
 * those fields before the player reads them leaves a response that describes a
 * video with no ad breaks, so the player plays the video.
 *
 * This is the technique uBlock Origin implements as `json-prune`, but the
 * objection recorded in `core/src/parser/mod.rs` is to *remotely supplied*
 * executable code arriving through the filter-list update channel — not to the
 * technique. This module is first-party, in-tree and audited like any other
 * source file, so it carries none of that risk. Filter lists still cannot
 * inject code.
 *
 * It runs in the page's MAIN world because it has to replace `JSON.parse` and
 * `Response.prototype.json` as the page sees them; an isolated content script
 * has its own copies and could not.
 */

/** Fields naming ad breaks in an InnerTube player response. */
const AD_FIELDS = [
  'adPlacements',
  'adSlots',
  'playerAds',
  'adBreakHeartbeatParams',
] as const;

/**
 * Inert until the isolated content script confirms filtering is on for this
 * page. Hooks install at document_start regardless, because the player
 * response can arrive at any time, but they pass values through untouched
 * until then. Defaulting to inactive means a disabled extension, a pause or an
 * allowlisted domain is honoured even though this world cannot read settings.
 */
let active = false;
let pruned = 0;
/**
 * Per-request logging is opt-in everywhere else in this project (§17), and a
 * page like YouTube's home feed prunes continuously, so announcing every prune
 * would both flood the console and log browsing activity by default. One line
 * per page is enough to tell a working install from a broken one.
 */
let announced = false;

/** True for objects that look like a player response carrying ad decisions. */
function carriesAdDecisions(value: unknown): value is Record<string, unknown> {
  if (value === null || typeof value !== 'object') return false;
  const record = value as Record<string, unknown>;
  if (AD_FIELDS.some((field) => field in record)) return true;
  const config = record.playerConfig as Record<string, unknown> | undefined;
  return config !== undefined && config !== null && 'adPlacementConfig' in config;
}

/** Strip ad decisions in place. Returns the number of fields removed. */
function pruneAdDecisions(record: Record<string, unknown>): number {
  let removed = 0;
  for (const field of AD_FIELDS) {
    if (field in record) {
      delete record[field];
      removed += 1;
    }
  }
  const config = record.playerConfig as Record<string, unknown> | undefined;
  if (config !== undefined && config !== null && 'adPlacementConfig' in config) {
    delete config.adPlacementConfig;
    removed += 1;
  }
  return removed;
}

/**
 * Walk a decoded response looking for player responses to prune.
 *
 * Bounded deliberately: YouTube nests the player response at a handful of
 * known depths, and an unbounded walk over every parsed object on the page
 * would be a performance problem on a site this JSON-heavy.
 */
function scrub(value: unknown, depth = 0): unknown {
  if (!active || value === null || typeof value !== 'object' || depth > 3) return value;

  if (carriesAdDecisions(value)) {
    const removed = pruneAdDecisions(value as Record<string, unknown>);
    if (removed > 0) {
      pruned += removed;
      if (!announced) {
        announced = true;
        console.info(
          'RatBlocker: pruning ad decisions from YouTube player responses ' +
            '(further prunes on this page are silent)',
        );
      }
    }
    return value;
  }

  if (Array.isArray(value)) {
    for (const entry of value) scrub(entry, depth + 1);
    return value;
  }

  // Only descend through the wrappers YouTube actually uses, rather than every
  // key of every object that passes through JSON.parse.
  for (const key of ['playerResponse', 'player', 'response', 'contents', 'args']) {
    const child = (value as Record<string, unknown>)[key];
    if (child !== undefined) scrub(child, depth + 1);
  }
  return value;
}

function install(): void {
  // 1. JSON.parse — the inline `ytInitialPlayerResponse` bootstrap and most
  //    InnerTube navigations go through here.
  const nativeParse = JSON.parse;
  JSON.parse = function parse(this: unknown, text: string, reviver?: Parameters<typeof JSON.parse>[1]) {
    const value = nativeParse.call(this, text, reviver as never);
    try {
      return scrub(value);
    } catch {
      return value;
    }
  } as typeof JSON.parse;

  // 2. Response.prototype.json — `fetch`-based player requests.
  const nativeJson = Response.prototype.json;
  Response.prototype.json = function json(this: Response) {
    return nativeJson.call(this).then((value: unknown) => {
      try {
        return scrub(value);
      } catch {
        return value;
      }
    });
  };

  // 3. `ytInitialPlayerResponse` is assigned directly by an inline script on a
  //    cold load, without passing through either hook above.
  let shadow: unknown;
  try {
    Object.defineProperty(window, 'ytInitialPlayerResponse', {
      configurable: true,
      get: () => shadow,
      set: (value: unknown) => {
        try {
          shadow = scrub(value);
        } catch {
          shadow = value;
        }
      },
    });
  } catch {
    // Another extension may already own the property; the hooks above still
    // cover navigations within the site.
  }
}

// The isolated-world half reports whether filtering applies here. Only
// "enable" is honoured: a page that forged this message could at worst turn
// blocking on for itself, never off.
window.addEventListener('message', (event: MessageEvent) => {
  if (event.source !== window) return;
  const data = event.data as { source?: string; enable?: boolean } | null;
  if (data === null || typeof data !== 'object') return;
  if (data.source !== 'ratblocker-streaming' || data.enable !== true) return;
  active = true;
});

// A page-world counter the diagnostics panel can read on demand, so the count
// is available without logging browsing activity as it happens.
Object.defineProperty(window, '__ratblockerPruned', {
  configurable: true,
  get: () => pruned,
});

install();
