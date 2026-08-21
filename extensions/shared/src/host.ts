/**
 * The parts of the extension that are identical on Chromium and Firefox:
 * engine lifecycle, settings, per-tab state, cosmetic CSS and the message
 * protocol behind the popup.
 *
 * Network interception is *not* here. Chromium must use declarativeNetRequest
 * and Firefox uses blocking webRequest, so each platform supplies its own
 * adapter and shares everything else (§11).
 */

import { api } from './browser.js';
import type { Message, Response, StatusReport } from './messaging.js';
import {
  isPaused,
  loadSettings,
  normalizeDomain,
  saveSettings,
  toEngineConfig,
  type Settings,
} from './settings.js';
import { Statistics } from './statistics.js';
import type { FilterResult } from './types.js';
import { RatBlockerEngine } from './wasm.js';

/** Longest pause the UI offers, as a guard against an accidental forever-off. */
export const MAX_PAUSE_SECONDS = 24 * 60 * 60;

/** What the build compiled in, written by `build.mjs`. */
export interface RuleCounts {
  networkRules: number;
  networkSource: 'declarativeNetRequest' | 'webRequest';
  cosmeticRules: number;
  collapsedDomains: number;
}

export interface HostOptions {
  /** Extension-relative path to the compiled WebAssembly core. */
  wasmPath: string;
  /** Extension-relative path to the compiled rule database. */
  databasePath: string;
  /** Called after the engine or settings change, so the adapter can re-sync. */
  onEngineChanged?: (host: ExtensionHost) => void | Promise<void>;
}

export class ExtensionHost {
  engine: RatBlockerEngine | null = null;
  engineError: string | null = null;
  settings: Settings;
  counts: RuleCounts | null = null;
  readonly stats = new Statistics();

  private pauseTimer: ReturnType<typeof setTimeout> | null = null;
  private readonly cssCache = new Map<string, string>();

  private constructor(
    private readonly options: HostOptions,
    settings: Settings,
  ) {
    this.settings = settings;
  }

  static async start(options: HostOptions): Promise<ExtensionHost> {
    const settings = await loadSettings();
    const host = new ExtensionHost(options, settings);
    host.stats.setEnabled(settings.privacy.statisticsEnabled);
    await host.loadCounts();
    await host.buildEngine();
    host.schedulePauseExpiry();
    return host;
  }

  /** Read the compiled rule counts written alongside the database. */
  private async loadCounts(): Promise<void> {
    try {
      const response = await fetch(api.runtime.getURL('rules/counts.json'));
      this.counts = response.ok ? ((await response.json()) as RuleCounts) : null;
    } catch {
      // Not fatal: the popup falls back to what the engine can tell it.
      this.counts = null;
    }
  }

  /** Load the database and construct the core engine. */
  private async buildEngine(): Promise<void> {
    try {
      const url = api.runtime.getURL(this.options.databasePath);
      const response = await fetch(url);
      if (!response.ok) {
        throw new Error(`cannot read ${this.options.databasePath} (${response.status})`);
      }
      const database = new Uint8Array(await response.arrayBuffer());
      this.engine?.dispose();
      this.engine = await RatBlockerEngine.create(
        api.runtime.getURL(this.options.wasmPath),
        database,
        toEngineConfig(this.settings),
        this.settings.customRules,
      );
      this.engineError = null;
      this.cssCache.clear();
    } catch (error) {
      // A broken database must not take the extension down with it; the popup
      // surfaces the failure instead of the user seeing silent non-filtering.
      this.engine = null;
      this.engineError = error instanceof Error ? error.message : String(error);
      console.error('RatBlocker: engine failed to start', error);
    }
  }

  get filteringActive(): boolean {
    return this.settings.enabled && !isPaused(this.settings) && this.engine !== null;
  }

  /** Push configuration into the running engine without rebuilding indexes. */
  private applyConfig(): void {
    this.cssCache.clear();
    if (!this.engine) return;
    try {
      this.engine.setConfig(toEngineConfig(this.settings));
    } catch (error) {
      console.error('RatBlocker: rejected configuration', error);
    }
  }

  async persist(options: { rebuild?: boolean } = {}): Promise<void> {
    await saveSettings(this.settings);
    this.stats.setEnabled(this.settings.privacy.statisticsEnabled);
    if (options.rebuild) {
      await this.buildEngine();
    } else {
      this.applyConfig();
    }
    this.schedulePauseExpiry();
    await this.options.onEngineChanged?.(this);
  }

  /** Resume automatically when a pause elapses, without waiting for a click. */
  private schedulePauseExpiry(): void {
    if (this.pauseTimer !== null) {
      clearTimeout(this.pauseTimer);
      this.pauseTimer = null;
    }
    const until = this.settings.pausedUntil;
    if (until === null) return;
    const remaining = until - Date.now();
    if (remaining <= 0) {
      this.settings.pausedUntil = null;
      void this.persist();
      return;
    }
    this.pauseTimer = setTimeout(() => {
      this.settings.pausedUntil = null;
      void this.persist();
    }, Math.min(remaining, MAX_PAUSE_SECONDS * 1000));
  }

  /** Cosmetic stylesheet for a page, memoized per origin. */
  cssFor(pageUrl: string): string {
    if (!this.filteringActive || !this.engine) return '';
    let key: string;
    try {
      key = new URL(pageUrl).origin;
    } catch {
      return '';
    }
    const cached = this.cssCache.get(key);
    if (cached !== undefined) return cached;
    const css = this.engine.cosmeticCss(pageUrl);
    // Bound the cache; a long browsing session visits many origins.
    if (this.cssCache.size > 200) this.cssCache.clear();
    this.cssCache.set(key, css);
    return css;
  }

  isAllowlisted(host: string): boolean {
    return this.settings.allowlist.some(
      (d) => host === d || host.endsWith(`.${d}`),
    );
  }

  /**
   * Evaluate a newly created tab/window with popup context.
   *
   * Browser network APIs expose the destination URL but cannot express
   * EasyList's `$popup` condition. Keeping this decision in the shared core
   * gives Chromium and Firefox identical full-URL and first-party matching.
   */
  evaluatePopup(targetUrl: string, sourceUrl: string | null): FilterResult | null {
    if (!this.filteringActive || this.engine === null) return null;
    try {
      return this.engine.evaluate({
        request_url: targetUrl,
        source_url: sourceUrl,
        application_id: null,
        resource_type: 'document',
        is_popup: true,
      });
    } catch (error) {
      console.error('RatBlocker: popup evaluation failed', error);
      return null;
    }
  }

  async handleMessage(message: Message, sender?: chrome.runtime.MessageSender): Promise<Response> {
    try {
      switch (message.type) {
        case 'getStatus':
          return { ok: true, status: await this.status(message.tabId) };

        case 'setEnabled':
          this.settings.enabled = message.enabled;
          this.settings.pausedUntil = null;
          await this.persist();
          return { ok: true };

        case 'pause': {
          const seconds = Math.min(
            Math.max(Math.floor(message.durationSeconds), 1),
            MAX_PAUSE_SECONDS,
          );
          this.settings.pausedUntil = Date.now() + seconds * 1000;
          await this.persist();
          return { ok: true };
        }

        case 'resume':
          this.settings.pausedUntil = null;
          await this.persist();
          return { ok: true };

        case 'allowlistAdd': {
          const domain = normalizeDomain(message.domain);
          if (domain === null) return { ok: false, error: 'not a valid domain' };
          if (!this.settings.allowlist.includes(domain)) {
            this.settings.allowlist.push(domain);
            this.settings.allowlist.sort();
          }
          await this.persist();
          return { ok: true };
        }

        case 'allowlistRemove': {
          const domain = normalizeDomain(message.domain) ?? message.domain;
          this.settings.allowlist = this.settings.allowlist.filter((d) => d !== domain);
          await this.persist();
          return { ok: true };
        }

        case 'getSettings':
          return { ok: true, settings: this.settings };

        case 'saveSettings': {
          const rebuild = message.settings.customRules !== this.settings.customRules;
          this.settings = message.settings;
          await this.persist({ rebuild });
          return { ok: true };
        }

        case 'getCosmetic': {
          const css = this.cssFor(message.url);
          return { ok: true, css, count: css === '' ? 0 : css.split(',').length };
        }

        /**
         * Does filtering apply to this page right now? Asked by the content
         * script on behalf of the MAIN-world pruner, which cannot read
         * settings itself. Answering false (or not at all) leaves it inert.
         */
        case 'shouldFilter': {
          let hostname: string;
          try {
            hostname = new URL(message.url).hostname;
          } catch {
            return { ok: true, filtering: false };
          }
          return {
            ok: true,
            filtering: this.filteringActive && !this.isAllowlisted(hostname),
          };
        }

        case 'resetStatistics':
          this.stats.reset();
          return { ok: true };

        case 'getDiagnostics':
          return { ok: true, diagnostics: this.diagnostics() };

        default: {
          const exhaustive: never = message;
          return { ok: false, error: `unknown message ${JSON.stringify(exhaustive)}` };
        }
      }
    } catch (error) {
      return { ok: false, error: error instanceof Error ? error.message : String(error) };
    } finally {
      void sender;
    }
  }

  private async status(tabId?: number): Promise<StatusReport> {
    let host: string | null = null;
    let resolvedTabId = tabId;
    try {
      const tabs = await api.tabs.query({ active: true, currentWindow: true });
      const tab = tabs[0];
      if (tab?.url !== undefined) {
        const url = new URL(tab.url);
        // Only a real web page has a site to allowlist. Without this the popup
        // happily shows the host of a `chrome-extension://` or `about:` page,
        // which is meaningless and offers an allowlist toggle that does
        // nothing.
        if (url.protocol === 'http:' || url.protocol === 'https:') {
          host = url.hostname;
        }
      }
      resolvedTabId ??= tab?.id;
    } catch {
      // A popup opened with no active tab is not an error.
    }
    return {
      enabled: this.settings.enabled,
      paused: isPaused(this.settings),
      pausedUntil: this.settings.pausedUntil,
      host,
      hostAllowlisted: host !== null && this.isAllowlisted(host),
      blockedOnTab: this.stats.forTab(resolvedTabId),
      blockedTotal: this.stats.blockedTotal,
      // The compiled counts, not the engine's: on Chromium the engine holds
      // only cosmetic rules while declarativeNetRequest does the blocking.
      rulesLoaded: this.counts?.networkRules ?? this.engine?.stats().rules ?? 0,
      cosmeticRules: this.counts?.cosmeticRules ?? 0,
      engineReady: this.engine !== null,
      engineError: this.engineError,
    };
  }

  diagnostics(): Record<string, unknown> {
    return {
      coreVersion: this.engine?.version ?? null,
      engineReady: this.engine !== null,
      engineError: this.engineError,
      statistics: {
        enabled: this.stats.isEnabled,
        blockedTotal: this.stats.blockedTotal,
      },
      settings: {
        enabled: this.settings.enabled,
        paused: isPaused(this.settings),
        allowlistSize: this.settings.allowlist.length,
        customRuleLines: this.settings.customRules
          .split('\n')
          .filter((l) => l.trim() !== '').length,
      },
      counts: this.counts,
      engine: this.engine?.stats() ?? null,
    };
  }
}
