/**
 * Firefox network adapter.
 *
 * Gecko still offers blocking `webRequest`, so Firefox runs the full core:
 * every request is evaluated by the same Rust engine the Linux daemon uses,
 * with no loss of EasyList fidelity. Chromium cannot do this under MV3, which
 * is why the two adapters differ (§14).
 */

import { api } from '../../shared/src/browser.js';
import { ExtensionHost } from '../../shared/src/host.js';
import { scheduleUpdateChecks } from '../../shared/src/updates.js';
import type { Message } from '../../shared/src/messaging.js';
import { resolveRedirect } from '../../shared/src/redirects.js';
import { resourceTypeFromBrowser } from '../../shared/src/types.js';

let host: ExtensionHost | null = null;

/** Schemes the extension should never touch. */
function isFilterable(url: string): boolean {
  return url.startsWith('http:') || url.startsWith('https:') || url.startsWith('ws');
}

function onBeforeRequest(
  details: chrome.webRequest.WebRequestBodyDetails & {
    originUrl?: string;
    documentUrl?: string;
  },
): chrome.webRequest.BlockingResponse {
  if (host === null || !host.filteringActive || host.engine === null) return {};
  if (!isFilterable(details.url)) return {};

  // For a top-level navigation there is no source document, and the page's own
  // URL must not be treated as its own initiator.
  const isTopLevel = details.type === 'main_frame';
  const source = isTopLevel ? null : (details.documentUrl ?? details.originUrl ?? null);

  let result;
  try {
    result = host.engine.evaluate({
      request_url: details.url,
      source_url: source,
      application_id: null,
      resource_type: resourceTypeFromBrowser(details.type),
      is_popup: false,
    });
  } catch (error) {
    // A failure here must never break the page.
    console.error('RatBlocker: evaluation failed', error);
    return {};
  }

  switch (result.decision) {
    case 'block':
      host.stats.recordBlock(details.tabId);
      updateBadge(details.tabId);
      return { cancel: true };

    case 'redirect': {
      const target = result.redirect_to ?? '';
      const url = resolveRedirect(target);
      host.stats.recordBlock(details.tabId);
      updateBadge(details.tabId);
      // An unrecognised stand-in falls back to blocking rather than passing
      // the request through.
      return url === null ? { cancel: true } : { redirectUrl: url };
    }

    case 'remove_parameters': {
      const rewritten = result.rewritten_url;
      if (rewritten !== undefined && rewritten !== null && rewritten !== details.url) {
        return { redirectUrl: rewritten };
      }
      return {};
    }

    default:
      return {};
  }
}

function updateBadge(tabId: number): void {
  if (host === null || !host.stats.isEnabled || tabId < 0) return;
  const count = host.stats.forTab(tabId);
  void api.browserAction.setBadgeText({
    tabId,
    text: count === 0 ? '' : String(count),
  });
}

/** Inject cosmetic CSS as early as the browser allows. */
async function injectCosmetic(tabId: number, frameId: number, url: string): Promise<void> {
  if (host === null || !host.filteringActive) return;
  const css = host.cssFor(url);
  if (css === '') return;
  try {
    await api.tabs.insertCSS(tabId, {
      code: css,
      frameId,
      runAt: 'document_start',
      // User-origin styles cannot be overridden by the page's own stylesheet.
      cssOrigin: 'user',
    } as chrome.tabs.InjectDetails);
  } catch {
    // The tab may have navigated away already; not an error worth surfacing.
  }
}

/** Close a new tab/window when a filter rule matches it specifically as a popup. */
async function inspectPopup(
  details: chrome.webNavigation.WebNavigationSourceCallbackDetails,
): Promise<void> {
  if (host === null) return;
  let sourceUrl: string | null = null;
  try {
    sourceUrl = (await api.tabs.get(details.sourceTabId)).url ?? null;
  } catch {
    // The opener may have closed before this event was handled.
  }
  const result = host.evaluatePopup(details.url, sourceUrl);
  if (result?.decision !== 'block' && result?.decision !== 'redirect') return;

  try {
    await api.tabs.remove(details.tabId);
    host.stats.recordBlock(details.sourceTabId);
    updateBadge(details.sourceTabId);
  } catch {
    // The target may already have been closed by the browser or the user.
  }
}

async function main(): Promise<void> {
  host = await ExtensionHost.start({
    wasmPath: 'core/ratblocker.wasm',
    databasePath: 'rules/rules.rbdb',
  });

  api.webRequest.onBeforeRequest.addListener(
    onBeforeRequest as never,
    { urls: ['<all_urls>'] },
    ['blocking'],
  );

  api.webNavigation.onCommitted.addListener((details) => {
    void injectCosmetic(details.tabId, details.frameId, details.url);
  });
  api.webNavigation.onCreatedNavigationTarget.addListener((details) => {
    void inspectPopup(details);
  });

  api.tabs.onRemoved.addListener((tabId) => host?.stats.clearTab(tabId));
  api.tabs.onUpdated.addListener((tabId, changeInfo) => {
    if (changeInfo.status === 'loading' && changeInfo.url !== undefined) {
      host?.stats.clearTab(tabId);
      updateBadge(tabId);
    }
  });

  api.runtime.onMessage.addListener(
    (message: Message, sender, sendResponse: (r: unknown) => void) => {
      if (host === null) {
        sendResponse({ ok: false, error: 'engine not started' });
        return false;
      }
      void host.handleMessage(message, sender).then(sendResponse);
      // Keep the channel open for the asynchronous reply.
      return true;
    },
  );

  // Self-hosted installs update through the browser, but only if it is
  // asked to look; its own poll interval can be a day away.
  scheduleUpdateChecks();

  void api.browserAction.setBadgeBackgroundColor({ color: '#8b1e3f' });
  console.info(`RatBlocker: core ${host.engine?.version ?? 'unavailable'} ready`);
}

void main();
