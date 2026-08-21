/**
 * Minimal Marionette client.
 *
 * Firefox 153 no longer speaks the Chrome DevTools Protocol, so the Firefox
 * harness drives the browser over Marionette instead. The wire format is
 * `<byte-length>:<json>` framing around `[type, id, command, params]`.
 */

import { connect as netConnect } from 'node:net';

export async function connect(port, host = '127.0.0.1', timeoutMs = 30000) {
  const socket = await new Promise((resolve, reject) => {
    const deadline = Date.now() + timeoutMs;
    const attempt = () => {
      const s = netConnect({ port, host });
      s.once('connect', () => resolve(s));
      s.once('error', (error) => {
        if (Date.now() > deadline) reject(error);
        else setTimeout(attempt, 250);
      });
    };
    attempt();
  });

  let buffer = Buffer.alloc(0);
  const pending = new Map();
  let handshake = null;
  const handshakeWaiters = [];
  let nextId = 1;

  socket.on('data', (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    for (;;) {
      const colon = buffer.indexOf(0x3a); // ':'
      if (colon === -1) return;
      const length = Number(buffer.subarray(0, colon).toString('ascii'));
      if (!Number.isFinite(length)) throw new Error('malformed Marionette frame');
      const start = colon + 1;
      if (buffer.length < start + length) return;
      const payload = JSON.parse(buffer.subarray(start, start + length).toString('utf8'));
      buffer = buffer.subarray(start + length);

      if (!Array.isArray(payload)) {
        // The initial handshake is a bare object.
        handshake = payload;
        for (const w of handshakeWaiters.splice(0)) w(payload);
        continue;
      }
      const [, id, error, result] = payload;
      const entry = pending.get(id);
      if (entry === undefined) continue;
      pending.delete(id);
      if (error !== null && error !== undefined) {
        entry.reject(new Error(`${error.error}: ${error.message}`));
      } else {
        entry.resolve(result);
      }
    }
  });

  await new Promise((resolve) => {
    if (handshake !== null) resolve(handshake);
    else handshakeWaiters.push(resolve);
  });

  return {
    send(command, params = {}) {
      const id = nextId++;
      const frame = JSON.stringify([0, id, command, params]);
      socket.write(`${Buffer.byteLength(frame)}:${frame}`);
      return new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
    },
    close() {
      socket.destroy();
    },
  };
}
