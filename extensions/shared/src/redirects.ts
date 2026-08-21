/**
 * Neutral stand-ins for blocked resources.
 *
 * The canonical table lives in `core/src/rule_engine/dnr.rs`; the filter
 * compiler writes these files into `dist/redirects/` and the build script
 * copies them into both extensions. A compatibility test asserts this map and
 * the Rust one stay in step.
 *
 * An unknown token falls back to blocking, which is the safe direction: the
 * request still never reaches the network.
 */

import { api } from './browser.js';

export const REDIRECT_FILES: Record<string, string> = {
  noopjs: 'noop.js',
  'noop.js': 'noop.js',
  'blank-js': 'noop.js',
  noopframe: 'noop.html',
  noophtml: 'noop.html',
  'noop.html': 'noop.html',
  'blank-html': 'noop.html',
  noopcss: 'noop.css',
  'noop.css': 'noop.css',
  'blank-css': 'noop.css',
  'noop.txt': 'noop.txt',
  'blank-text': 'noop.txt',
  'noop.gif': 'noop.gif',
  '1x1.gif': 'noop.gif',
  'blank-gif': 'noop.gif',
  'noop.mp4': 'noop.mp4',
  'blank-mp4': 'noop.mp4',
  'noop.mp3': 'noop.mp3',
  'blank-mp3': 'noop.mp3',
};

/** Resolve a redirect token to an extension URL, or null if unknown. */
export function resolveRedirect(target: string): string | null {
  const file = REDIRECT_FILES[target] ?? REDIRECT_FILES[target.toLowerCase()];
  return file === undefined ? null : api.runtime.getURL(`redirects/${file}`);
}
