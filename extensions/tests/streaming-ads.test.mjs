/**
 * Behavioural tests for the MAIN-world streaming pruner.
 *
 * These run the *bundled* script, not the TypeScript source, so they cover
 * what actually ships. The script is evaluated in a `vm` context holding
 * stand-ins for the browser globals it patches, which keeps it from replacing
 * this process's real `JSON.parse`.
 *
 * What these can catch: a regression in our own pruning logic or activation
 * handshake. What they cannot catch: YouTube renaming its ad fields, which is
 * external and needs the manual check in docs/scope.md.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createContext, runInContext } from 'node:vm';
import { build } from 'esbuild';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));

const bundled = (
  await build({
    entryPoints: [join(here, '../shared/src/streaming-ads.ts')],
    bundle: true,
    write: false,
    format: 'iife',
    target: 'es2022',
    platform: 'browser',
    logLevel: 'silent',
  })
).outputFiles[0].text;

/** A player response as YouTube delivers it, with ad breaks present. */
function playerResponseFixture() {
  return {
    responseContext: { visitorData: 'x' },
    playabilityStatus: { status: 'OK' },
    streamingData: { formats: [{ itag: 18 }] },
    videoDetails: { videoId: 'abc', title: 'Test video' },
    playerConfig: { audioConfig: {}, adPlacementConfig: { kind: 'AD_PLACEMENT' } },
    adPlacements: [{ adPlacementRenderer: {} }],
    adSlots: [{ adSlotRenderer: {} }],
    playerAds: [{ playerLegacyDesktopWatchAdsRenderer: {} }],
    adBreakHeartbeatParams: 'deadbeef',
  };
}

/** Load the bundle into a fresh fake page. Returns handles for driving it. */
function loadPruner() {
  const listeners = [];
  const realParse = JSON.parse.bind(JSON);

  const context = {
    console: { info() {}, warn() {}, error() {} },
    // A private JSON so the script cannot patch this process's global.
    JSON: { parse: realParse, stringify: JSON.stringify.bind(JSON) },
    Object,
    Promise,
    Array,
    Response: class FakeResponse {
      constructor(body) { this._body = body; }
      json() { return Promise.resolve(this._body); }
    },
  };
  context.window = context;
  context.addEventListener = (type, fn) => {
    if (type === 'message') listeners.push(fn);
  };
  createContext(context);
  runInContext(bundled, context);

  // `createContext` contextifies the global behind a proxy, so the object we
  // passed in is NOT the `window` the script sees. The script's message guard
  // compares against its own `window`, so events must carry that reference or
  // they are correctly rejected as cross-window.
  const scriptWindow = runInContext('window', context);

  return {
    context,
    window: scriptWindow,
    parse: (value) => context.JSON.parse(JSON.stringify(value)),
    enable() {
      for (const fn of listeners) {
        fn({ source: scriptWindow, data: { source: 'ratblocker-streaming', enable: true } });
      }
    },
    forge(data) {
      for (const fn of listeners) fn({ source: scriptWindow, data });
    },
  };
}

const AD_FIELDS = ['adPlacements', 'adSlots', 'playerAds', 'adBreakHeartbeatParams'];

test('prunes every ad decision field once activated', () => {
  const p = loadPruner();
  p.enable();
  const result = p.parse(playerResponseFixture());
  for (const field of AD_FIELDS) {
    assert.ok(!(field in result), `${field} should have been removed`);
  }
  assert.ok(!('adPlacementConfig' in result.playerConfig), 'adPlacementConfig should be removed');
});

test('leaves the actual video intact', () => {
  const p = loadPruner();
  p.enable();
  const result = p.parse(playerResponseFixture());
  assert.equal(result.videoDetails.title, 'Test video');
  assert.deepEqual(result.streamingData.formats, [{ itag: 18 }]);
  assert.equal(result.playabilityStatus.status, 'OK');
  assert.ok('audioConfig' in result.playerConfig, 'unrelated playerConfig keys must survive');
});

test('stays inert until activated, so a disabled extension does not prune', () => {
  const p = loadPruner();
  // No enable() call: this models pause, allowlist, or a sleeping background.
  const result = p.parse(playerResponseFixture());
  for (const field of AD_FIELDS) {
    assert.ok(field in result, `${field} must survive while inactive`);
  }
});

test('a page cannot switch pruning off by forging a message', () => {
  const p = loadPruner();
  p.enable();
  p.forge({ source: 'ratblocker-streaming', enable: false });
  const result = p.parse(playerResponseFixture());
  assert.ok(!('adPlacements' in result), 'forged disable must not deactivate pruning');
});

test('unrelated JSON passes through untouched', () => {
  const p = loadPruner();
  p.enable();
  const payload = { items: [1, 2, 3], nested: { keep: true } };
  assert.deepEqual(p.parse(payload), payload);
});

test('prunes a player response nested under a known wrapper', () => {
  const p = loadPruner();
  p.enable();
  const result = p.parse({ playerResponse: playerResponseFixture() });
  assert.ok(!('adPlacements' in result.playerResponse), 'nested response should be pruned');
});

test('ytInitialPlayerResponse assignment is pruned', () => {
  const p = loadPruner();
  p.enable();
  p.window.ytInitialPlayerResponse = playerResponseFixture();
  const stored = p.window.ytInitialPlayerResponse;
  for (const field of AD_FIELDS) {
    assert.ok(!(field in stored), `${field} should be pruned on assignment`);
  }
  assert.equal(stored.videoDetails.videoId, 'abc');
});

test('Response.prototype.json is pruned', async () => {
  const p = loadPruner();
  p.enable();
  const response = new p.context.Response(playerResponseFixture());
  const body = await response.json();
  assert.ok(!('adPlacements' in body), 'fetch-delivered responses should be pruned');
});

test('exposes a diagnostics counter without logging each prune', () => {
  const p = loadPruner();
  p.enable();
  p.parse(playerResponseFixture());
  assert.ok(p.window.__ratblockerPruned > 0, 'counter should advance');
});
