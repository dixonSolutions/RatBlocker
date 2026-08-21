/**
 * Self-hosted update checks.
 *
 * An extension installed outside a store still updates through the browser's
 * own machinery, provided the packaged manifest names where to look:
 * `update_url` on Chromium, `browser_specific_settings.gecko.update_url` on
 * Gecko. `package.mjs` writes both, and generates the manifests they point at.
 *
 * Both engines poll on their own schedule — Chromium roughly every five hours,
 * Gecko once a day by default — which means a browser that is opened and
 * closed inside that window can go a long time without ever checking. So this
 * asks for a check explicitly at startup as well.
 *
 * `requestUpdateCheck` is rate-limited by the browser and throws when called
 * too often; that is not an error worth surfacing. Nothing here downloads or
 * applies anything itself. The browser verifies the signature and performs the
 * update; this only asks it to look.
 */

import { api } from './browser.js';

/** Chromium's own throttle is stricter than this; the guard is for our sake. */
const MIN_INTERVAL_MS = 6 * 60 * 60 * 1000;
const LAST_CHECK_KEY = 'lastUpdateCheck';

type UpdateCheckResult = { status: string; version?: string };

/**
 * Ask the browser to poll the update manifest.
 *
 * Returns what the browser said, or null when the check was skipped, throttled
 * or unsupported. An unpacked development build has no update_url, so this is
 * expected to do nothing there.
 */
export async function checkForUpdates(options: { force?: boolean } = {}): Promise<
  UpdateCheckResult | null
> {
  try {
    if (options.force !== true) {
      const stored = await api.storage.local.get(LAST_CHECK_KEY);
      const last = (stored[LAST_CHECK_KEY] as number | undefined) ?? 0;
      if (Date.now() - last < MIN_INTERVAL_MS) return null;
    }
    await api.storage.local.set({ [LAST_CHECK_KEY]: Date.now() });

    // The bundled Chrome types still describe the callback form of this API.
    // Both engines have returned a promise from it for years, and Gecko never
    // offered a callback, so the promise form is the only one worth calling.
    const request = api.runtime.requestUpdateCheck as unknown as
      | (() => Promise<{ status: string; version?: string } | string | undefined>)
      | undefined;
    if (typeof request !== 'function') return null;

    // Chromium resolves to { status, details }; Gecko resolves to a status
    // string. Normalise so callers do not have to care.
    const result = await request.call(api.runtime);
    if (result === undefined) return null;
    const normalised: UpdateCheckResult =
      typeof result === 'string' ? { status: result } : { status: result.status, version: result.version };

    if (normalised.status === 'update_available') {
      console.info(
        `RatBlocker: an update is available${normalised.version ? ` (${normalised.version})` : ''};` +
          ' the browser will install it',
      );
    }
    return normalised;
  } catch {
    // Throttled, offline, or no update_url in this build. All benign.
    return null;
  }
}

/**
 * Wire startup checks.
 *
 * `onStartup` covers a browser launch. On Chromium an MV3 worker is also
 * revived long after launch, so the call at registration time covers a worker
 * that woke up to handle something else. Both go through the interval guard,
 * so a browser that respawns the worker repeatedly does not poll repeatedly.
 */
export function scheduleUpdateChecks(): void {
  try {
    api.runtime.onStartup?.addListener(() => {
      void checkForUpdates();
    });
  } catch {
    // onStartup is absent in some contexts; the call below still covers us.
  }
  void checkForUpdates();
}
