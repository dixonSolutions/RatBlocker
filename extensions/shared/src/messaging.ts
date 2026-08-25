/** The message protocol between the popup, content scripts and the worker. */

import type { Settings } from './settings.js';

export interface StatusReport {
  enabled: boolean;
  paused: boolean;
  pausedUntil: number | null;
  /** Host of the active tab, when there is one. */
  host: string | null;
  hostAllowlisted: boolean;
  blockedOnTab: number;
  blockedTotal: number;
  /** Network rules actually enforcing, however this platform enforces them. */
  rulesLoaded: number;
  cosmeticRules: number;
  engineReady: boolean;
  /** Set when the engine failed to start, for the diagnostics panel. */
  engineError: string | null;
}

export type Message =
  | { type: 'getStatus'; tabId?: number }
  | { type: 'setEnabled'; enabled: boolean }
  | { type: 'pause'; durationSeconds: number }
  | { type: 'resume' }
  | { type: 'allowlistAdd'; domain: string }
  | { type: 'allowlistRemove'; domain: string }
  | { type: 'getSettings' }
  | { type: 'saveSettings'; settings: Settings }
  | { type: 'getCosmetic'; url: string }
  | { type: 'shouldFilter'; url: string }
  | { type: 'resetStatistics' }
  | { type: 'checkForUpdates' }
  | { type: 'getDiagnostics' };

export type Response =
  | { ok: true; status: StatusReport }
  | { ok: true; settings: Settings }
  | { ok: true; css: string; count: number }
  | { ok: true; filtering: boolean }
  | { ok: true; update: { status: string; version?: string } | null }
  | { ok: true; diagnostics: Record<string, unknown> }
  | { ok: true }
  | { ok: false; error: string };
