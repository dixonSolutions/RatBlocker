/**
 * Facts about the project that are not derivable from a build artifact.
 *
 * Anything countable — rule totals, database sizes, list versions — comes from
 * `build-facts.ts`, which is generated from the compiler's own output. This
 * file holds only prose and links.
 */

export const PROJECT = {
  name: 'RatBlocker',
  tagline: 'Local, private ad and tracker blocking.',
  repository: 'https://github.com/dixonSolutions/RatBlocker',
  license: 'GPL-3.0-or-later',
  chromiumExtensionId: 'mkkpcbjiinhopbipddkpjjaeffjmfnnb',
  firefoxExtensionId: 'ratblocker@ratblocker.github.io',

  /**
   * The add-on's home on addons.mozilla.org.
   *
   * `amoListed` is what the install button keys off, and it is false until a
   * listed submission has actually been accepted — the publish workflow flips
   * it when AMO returns a slug. Showing a button that leads to a 404 is worse
   * than showing no button, so until then the site offers the build-it-
   * yourself route only.
   */
  amoSlug: 'ratblocker',
  amoListed: true,
} as const;

/** Where a published add-on lives, derived from the slug AMO assigned. */
export const AMO = {
  listingUrl: `https://addons.mozilla.org/firefox/addon/${PROJECT.amoSlug}/`,
  latestXpiUrl: `https://addons.mozilla.org/firefox/downloads/latest/${PROJECT.amoSlug}/latest.xpi`,
} as const;

export interface Promise_ {
  title: string;
  detail: string;
}

/** The defaults from the privacy architecture, stated as commitments. */
export const PRIVACY_PROMISES: Promise_[] = [
  { title: 'No account', detail: 'Nothing to sign up for, and nothing to sign in to.' },
  { title: 'No cloud', detail: 'Filtering happens on your machine. Rules ship with the build.' },
  { title: 'No analytics', detail: 'No telemetry, no usage reporting, no crash pings.' },
  {
    title: 'No TLS interception',
    detail: 'RatBlocker never installs a certificate authority and never decrypts your traffic.',
  },
  {
    title: 'Statistics off by default',
    detail: 'Counters are local-only, opt-in, and record no URLs.',
  },
  {
    title: 'Logging off by default',
    detail: 'Request logging must be switched on deliberately, and stays on your disk.',
  },
];

export interface Limitation {
  scope: string;
  detail: string;
}

/**
 * Stated plainly, because a blocker that oversells itself is worse than one
 * that explains where it stops.
 */
export const LIMITATIONS: Limitation[] = [
  {
    scope: 'DNS filtering',
    detail:
      'Cannot block an ad served from the same domain as the content you want, because the only thing it sees is a hostname.',
  },
  {
    scope: 'System filtering',
    detail: 'Cannot hide an empty element a blocked ad leaves behind. Only a browser can do that.',
  },
  {
    scope: 'Browser extensions',
    detail: 'Cannot protect anything outside the browser.',
  },
  {
    scope: 'HTTPS',
    detail:
      'Hides full URLs from any system-level filter. That is a feature of the web, and RatBlocker will not break it to see through.',
  },
  {
    scope: 'Chromium MV3',
    detail:
      'Cannot express every EasyList rule. RatBlocker reports exactly what it could not translate rather than hiding the gap.',
  },
  {
    scope: 'In-stream video ads',
    detail: 'Often indistinguishable from the video itself, and are not reliably blockable.',
  },
];
