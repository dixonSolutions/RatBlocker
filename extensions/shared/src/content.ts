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

async function main(): Promise<void> {
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
