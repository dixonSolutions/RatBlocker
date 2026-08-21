/**
 * Cosmetic filtering inside the page.
 *
 * The background injects the stylesheet at navigation time, which is earlier
 * and cheaper. This script is the fallback for frames that injection cannot
 * reach — `about:blank` and `srcdoc` iframes, and any frame that committed
 * while an MV3 service worker was asleep — and it is what recovers page layout
 * where an ad slot leaves a gap behind.
 */

import { api } from './browser.js';

const STYLE_ID = 'ratblocker-cosmetic';

async function requestStylesheet(): Promise<string> {
  try {
    const response = (await api.runtime.sendMessage({
      type: 'getCosmetic',
      url: location.href,
    })) as { ok: boolean; css?: string } | undefined;
    return response?.ok === true ? (response.css ?? '') : '';
  } catch {
    // The background may be restarting; nothing to do but skip this pass.
    return '';
  }
}

function inject(css: string): void {
  if (css === '' || document.getElementById(STYLE_ID) !== null) return;
  const style = document.createElement('style');
  style.id = STYLE_ID;
  style.textContent = css;
  // `documentElement` exists at document_start; `head` may not yet.
  (document.head ?? document.documentElement).append(style);
}

/**
 * Activate the MAIN-world streaming pruner, which starts inert.
 *
 * It cannot read settings from the page world, so the decision is made here
 * and sent across. Only activation is ever sent: staying silent leaves
 * pruning off, so a disabled extension, an active pause or an allowlisted
 * domain all result in no message and no pruning.
 */
async function activateStreamingPruner(): Promise<void> {
  if (!/(^|\.)(youtube|youtube-nocookie)\.com$/.test(location.hostname)) return;
  try {
    const response = (await api.runtime.sendMessage({
      type: 'shouldFilter',
      url: location.href,
    })) as { ok: boolean; filtering?: boolean } | undefined;
    if (response?.ok !== true || response.filtering !== true) return;
  } catch {
    // Background asleep or restarting: leave the pruner inert rather than
    // guessing that filtering applies.
    return;
  }
  window.postMessage({ source: 'ratblocker-streaming', enable: true }, location.origin);
}

async function main(): Promise<void> {
  void activateStreamingPruner();

  // The background already injected for ordinary frames; only step in when it
  // demonstrably has not.
  const alreadyStyled = document.adoptedStyleSheets.length > 0;
  if (alreadyStyled && document.getElementById(STYLE_ID) !== null) return;

  const css = await requestStylesheet();
  if (css === '') return;

  if (document.documentElement !== null) {
    inject(css);
  } else {
    document.addEventListener('DOMContentLoaded', () => inject(css), { once: true });
  }
}

void main();
