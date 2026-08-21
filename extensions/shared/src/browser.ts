/**
 * One handle on the extension APIs.
 *
 * Firefox exposes promise-returning `browser.*`; Chromium exposes `chrome.*`,
 * which has returned promises for MV3 APIs since Chrome 88. Everything the
 * extension uses is available under both names with the same shape, so a
 * single alias is enough and no polyfill is bundled.
 */

declare const browser: typeof chrome | undefined;

export const api: typeof chrome =
  typeof browser !== 'undefined' && browser !== null ? (browser as typeof chrome) : chrome;

/** True when running on a Gecko-based browser. */
export const isFirefox: boolean =
  typeof navigator !== 'undefined' && navigator.userAgent.includes('Gecko/');
