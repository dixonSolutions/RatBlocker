/**
 * Chromium network adapter.
 *
 * MV3 removed blocking `webRequest`, so network filtering here is declarative:
 * the compiler emits static `declarativeNetRequest` rulesets, and this worker
 * maintains the dynamic rules that depend on user state — the allowlist and
 * the user's own filters.
 *
 * The core still runs, as WebAssembly, for cosmetic filtering and for
 * compiling user rules, so user rules go through the same parser as everything
 * else instead of a second, divergent implementation (§11).
 */

import { api } from '../../shared/src/browser.js';
import { ExtensionHost } from '../../shared/src/host.js';
import { scheduleUpdateChecks } from '../../shared/src/updates.js';
import type { Message } from '../../shared/src/messaging.js';

/** Dynamic rule id ranges, kept apart so each can be replaced independently. */
const ALLOWLIST_ID_BASE = 1;
const USER_RULE_ID_BASE = 100_000;
/** Must outrank every priority the compiler assigns to a static rule. */
const ALLOWLIST_PRIORITY = 1000;

let host: ExtensionHost | null = null;
let staticRulesetIds: string[] = [];

/** Read the ruleset ids the manifest declares, rather than hardcoding them. */
function manifestRulesetIds(): string[] {
  const manifest = api.runtime.getManifest() as chrome.runtime.Manifest & {
    declarative_net_request?: { rule_resources?: Array<{ id: string }> };
  };
  return manifest.declarative_net_request?.rule_resources?.map((r) => r.id) ?? [];
}

/**
 * Turn the allowlist into `allowAllRequests` rules.
 *
 * The condition matches the *document* request, which is what makes Chromium
 * exempt every subresource loaded inside that document too.
 */
function allowlistRules(domains: string[]): chrome.declarativeNetRequest.Rule[] {
  return domains.map((domain, i) => ({
    id: ALLOWLIST_ID_BASE + i,
    priority: ALLOWLIST_PRIORITY,
    action: { type: 'allowAllRequests' as chrome.declarativeNetRequest.RuleActionType },
    condition: {
      requestDomains: [domain],
      resourceTypes: [
        'main_frame',
        'sub_frame',
      ] as chrome.declarativeNetRequest.ResourceType[],
    },
  }));
}

/** Compile the user's own rules through the core's DNR converter. */
function userRules(text: string): chrome.declarativeNetRequest.Rule[] {
  if (host?.engine === undefined || host?.engine === null) return [];
  if (text.trim() === '') return [];
  try {
    const { rules, problems } = host.engine.compileDnr(text, USER_RULE_ID_BASE);
    if (problems.length > 0) {
      console.warn('RatBlocker: some user rules could not be applied', problems);
    }
    return rules as chrome.declarativeNetRequest.Rule[];
  } catch (error) {
    console.error('RatBlocker: failed to compile user rules', error);
    return [];
  }
}

/** Bring Chromium's rule state in line with the current settings. */
async function syncRules(): Promise<void> {
  if (host === null) return;
  const active = host.settings.enabled && !isPausedNow();

  // Static rulesets carry the subscriptions; switching them off is how the
  // extension is disabled or paused.
  try {
    await api.declarativeNetRequest.updateEnabledRulesets({
      enableRulesetIds: active ? staticRulesetIds : [],
      disableRulesetIds: active ? [] : staticRulesetIds,
    });
  } catch (error) {
    console.error('RatBlocker: could not update static rulesets', error);
  }

  const existing = await api.declarativeNetRequest.getDynamicRules();
  const addRules = active
    ? [...allowlistRules(host.settings.allowlist), ...userRules(host.settings.customRules)]
    : [];
  try {
    await api.declarativeNetRequest.updateDynamicRules({
      removeRuleIds: existing.map((r) => r.id),
      addRules,
    });
  } catch (error) {
    // One bad rule rejects the whole batch; fall back to the allowlist alone
    // so a malformed custom rule cannot switch off the user's allowlist.
    console.error('RatBlocker: dynamic rules rejected, retrying without user rules', error);
    try {
      await api.declarativeNetRequest.updateDynamicRules({
        removeRuleIds: existing.map((r) => r.id),
        addRules: active ? allowlistRules(host.settings.allowlist) : [],
      });
    } catch (fallbackError) {
      console.error('RatBlocker: dynamic rule update failed', fallbackError);
    }
  }
}

function isPausedNow(): boolean {
  const until = host?.settings.pausedUntil ?? null;
  return until !== null && Date.now() < until;
}

function updateBadge(tabId: number): void {
  if (host === null || !host.stats.isEnabled || tabId < 0) return;
  const count = host.stats.forTab(tabId);
  void api.action.setBadgeText({
    tabId,
    text: count === 0 ? '' : String(count),
  });
}

/** Inject cosmetic CSS for a committed navigation. */
async function injectCosmetic(tabId: number, frameId: number, url: string): Promise<void> {
  if (host === null || !host.filteringActive) return;
  const css = host.cssFor(url);
  if (css === '') return;
  try {
    await api.scripting.insertCSS({
      target: { tabId, frameIds: [frameId] },
      css,
      origin: 'USER',
    });
  } catch {
    // The frame may be gone, or be a page extensions may not touch.
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

/**
 * Count blocks.
 *
 * `declarativeNetRequest` does not report matches to a packed extension, so
 * blocked requests are observed through non-blocking `webRequest`, where a
 * DNR-blocked request surfaces as `net::ERR_BLOCKED_BY_CLIENT`.
 */
function watchBlocked(): void {
  api.webRequest.onErrorOccurred.addListener(
    (details) => {
      if (host === null || !host.stats.isEnabled) return;
      if (!details.error.includes('BLOCKED_BY_CLIENT')) return;
      host.stats.recordBlock(details.tabId);
      updateBadge(details.tabId);
    },
    { urls: ['<all_urls>'] },
  );
}

async function main(): Promise<void> {
  staticRulesetIds = manifestRulesetIds();

  host = await ExtensionHost.start({
    wasmPath: 'core/ratblocker.wasm',
    // Only cosmetic rules: Chromium's network filtering is declarative, so
    // shipping the full database would cost startup time for nothing.
    databasePath: 'rules/cosmetic.rbdb',
    onEngineChanged: syncRules,
  });

  await syncRules();
  watchBlocked();

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
      void api.action.setBadgeText({ tabId, text: '' });
    }
  });

  api.runtime.onMessage.addListener(
    (message: Message, sender, sendResponse: (r: unknown) => void) => {
      if (host === null) {
        sendResponse({ ok: false, error: 'engine not started' });
        return false;
      }
      void host.handleMessage(message, sender).then(sendResponse);
      return true;
    },
  );

  // Self-hosted installs update through the browser, but only if it is
  // asked to look; its own poll interval can be hours away.
  scheduleUpdateChecks();

  void api.action.setBadgeBackgroundColor({ color: '#8b1e3f' });
  console.info(`RatBlocker: core ${host.engine?.version ?? 'unavailable'} ready`);
}

void main();
