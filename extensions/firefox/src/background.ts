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
import type { Message } from '../../shared/src/messaging.js';
import { resolveRedirect } from '../../shared/src/redirects.js';
import { resourceTypeFromBrowser } from '../../shared/src/types.js';

let host: ExtensionHost | null = null;
const pendingPopups = new Map<number, PopupContext>();

interface PopupContext {
  sourceTabId: number;
  sourceUrl: Promise<string | null>;
}

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

async function popupSourceUrl(tabId: number): Promise<string | null> {
  try {
    return (await api.tabs.get(tabId)).url ?? null;
  } catch {
    // The opener may have closed before this event was handled.
    return null;
  }
}

/** Close a new tab/window when a filter rule matches it specifically as a popup. */
async function inspectPopup(
  tabId: number,
  targetUrl: string,
  context: PopupContext,
): Promise<void> {
  if (host === null) return;
  const result = host.evaluatePopup(targetUrl, await context.sourceUrl);
  if (result?.decision !== 'block' && result?.decision !== 'redirect') return;

  try {
    await api.tabs.remove(tabId);
    host.stats.recordBlock(context.sourceTabId);
    updateBadge(context.sourceTabId);
  } catch {
    // The target may already have been closed by the browser or the user.
  }
}

function inspectPendingPopup(tabId: number, url: string): void {
  const context = pendingPopups.get(tabId);
  if (context === undefined || !isFilterable(url)) return;
  pendingPopups.delete(tabId);
  void inspectPopup(tabId, url, context);
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
    if (details.frameId === 0) inspectPendingPopup(details.tabId, details.url);
  });
  api.webNavigation.onCreatedNavigationTarget.addListener((details) => {
    const context: PopupContext = {
      sourceTabId: details.sourceTabId,
      sourceUrl: popupSourceUrl(details.sourceTabId),
    };
    if (isFilterable(details.url)) {
      void inspectPopup(details.tabId, details.url, context);
    } else {
      pendingPopups.set(details.tabId, context);
    }
  });

  api.tabs.onRemoved.addListener((tabId) => {
    pendingPopups.delete(tabId);
    host?.stats.clearTab(tabId);
  });
  api.tabs.onUpdated.addListener((tabId, changeInfo) => {
    if (changeInfo.url !== undefined) inspectPendingPopup(tabId, changeInfo.url);
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

  void api.browserAction.setBadgeBackgroundColor({ color: '#8b1e3f' });
  console.info(`RatBlocker: core ${host.engine?.version ?? 'unavailable'} ready`);
}

void main();
