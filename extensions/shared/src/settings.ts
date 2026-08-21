/**
 * Extension-local settings.
 *
 * The shape mirrors `ratblocker_core::storage::Configuration` so that a
 * configuration exported from the Linux daemon and one exported from a browser
 * describe the same thing. Storage itself is `storage.local`, per §18.
 */

import { api } from './browser.js';
import type { EngineConfig } from './types.js';

export const SETTINGS_VERSION = 1;

export interface Subscription {
  id: string;
  title: string;
  enabled: boolean;
  /** Absent for the lists bundled with the extension. */
  url?: string;
  /** Third-party lists must be trusted explicitly before they are used. */
  trusted: boolean;
  lastUpdated?: number;
  ruleCount?: number;
}

export interface Settings {
  version: number;
  enabled: boolean;
  /** Epoch milliseconds; filtering resumes automatically after this. */
  pausedUntil: number | null;
  allowlist: string[];
  customRules: string;
  subscriptions: Subscription[];
  privacy: {
    statisticsEnabled: boolean;
  };
  updates: {
    automatic: boolean;
    intervalHours: number;
  };
}

export function defaultSettings(): Settings {
  return {
    version: SETTINGS_VERSION,
    enabled: true,
    pausedUntil: null,
    allowlist: [],
    customRules: '',
    subscriptions: [
      { id: 'easylist', title: 'EasyList', enabled: true, trusted: true },
      { id: 'easyprivacy', title: 'EasyPrivacy', enabled: true, trusted: true },
    ],
    // Off by default: no analytics, no logging (§17).
    privacy: { statisticsEnabled: false },
    updates: { automatic: true, intervalHours: 24 },
  };
}

const KEY = 'settings';

export async function loadSettings(): Promise<Settings> {
  const stored = await api.storage.local.get(KEY);
  const raw = stored[KEY] as Partial<Settings> | undefined;
  if (!raw) return defaultSettings();
  return migrate({ ...defaultSettings(), ...raw });
}

export async function saveSettings(settings: Settings): Promise<void> {
  await api.storage.local.set({ [KEY]: settings });
}

/** Versioned, additive migrations (§18). */
function migrate(settings: Settings): Settings {
  if (settings.version > SETTINGS_VERSION) {
    // A downgrade: keep the user's data but do not pretend to understand it.
    console.warn('RatBlocker: settings are newer than this build; using defaults');
    return defaultSettings();
  }
  settings.version = SETTINGS_VERSION;
  return settings;
}

/** True when filtering is switched off right now, pause included. */
export function isPaused(settings: Settings): boolean {
  return settings.pausedUntil !== null && Date.now() < settings.pausedUntil;
}

/** Project settings into what the core actually consults per request. */
export function toEngineConfig(settings: Settings): EngineConfig {
  return {
    allowlisted_domains: settings.allowlist,
    application_policies: {},
    enabled: settings.enabled && !isPaused(settings),
  };
}

/** Normalize user input into a bare hostname, or null if it is not one. */
export function normalizeDomain(input: string): string | null {
  let value = input.trim().toLowerCase();
  if (value === '') return null;
  if (value.includes('://')) {
    try {
      value = new URL(value).hostname;
    } catch {
      return null;
    }
  }
  value = value.replace(/^www\./, '').replace(/\.$/, '').split('/')[0];
  if (!/^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$/.test(value)) {
    return null;
  }
  return value;
}
