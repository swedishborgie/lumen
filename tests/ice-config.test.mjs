import { test } from 'node:test';
import assert from 'node:assert/strict';
import { peerConnectionConfig } from '../web/lumen-client.mjs';

test('SSH preserves forwarded TCP URL, credentials and relay-only policy', () => {
  const iceServers = [{ urls: 'turn:127.0.0.1:13478?transport=tcp', username: 'u', credential: 'p' }];
  assert.deepEqual(peerConnectionConfig({ iceServers, iceTransportPolicy: 'relay' }), {
    iceServers, iceTransportPolicy: 'relay', bundlePolicy: 'max-bundle',
  });
});
test('normal and intentionally empty ICE configurations have no public STUN fallback', () => {
  assert.deepEqual(peerConnectionConfig({ iceServers: [] }).iceServers, []);
  assert.equal(peerConnectionConfig({ iceServers: [] }).iceTransportPolicy, 'all');
});
test('invalid configuration and unusable relay-only configuration fail', () => {
  for (const cfg of [null, {}, { iceServers: [], iceTransportPolicy: 'invalid' },
    { iceServers: [], iceTransportPolicy: 'relay' },
    { iceServers: [{ urls: 'stun:example.org' }], iceTransportPolicy: 'relay' }]) {
    assert.throws(() => peerConnectionConfig(cfg));
  }
});

test('failed config fetch closes signaling without creating a peer connection', async t => {
  const { LumenClient } = await import('../web/lumen-client.mjs');
  let closed = false;
  let createdPeer = false;
  const saved = { WebSocket: globalThis.WebSocket, fetch: globalThis.fetch, RTCPeerConnection: globalThis.RTCPeerConnection };
  t.after(() => Object.assign(globalThis, saved));
  globalThis.WebSocket = class extends EventTarget {
    constructor() {
      super();
      queueMicrotask(() => this.dispatchEvent(new Event('open')));
    }
    close() { closed = true; }
  };
  globalThis.RTCPeerConnection = class { constructor() { createdPeer = true; } };
  globalThis.fetch = async () => ({ ok: false, status: 503 });
  const client = new LumenClient();
  const statuses = [];
  client.addEventListener('statuschange', event => statuses.push(event.detail));
  await client.connect('ws://localhost/ws/signal');
  assert.equal(client.state, 'idle');
  assert.equal(closed, true);
  assert.equal(createdPeer, false);
  assert.match(statuses.at(-1), /Connection failed:.*503/);
});
