// SPEC-OBS-004: exercise the exact JavaScript imported by Quickshell.
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { resolve } = require('node:path');
const { test } = require('node:test');
const vm = require('node:vm');
const status = vm.createContext({});
vm.runInContext(readFileSync(resolve(__dirname, '../Status.js'), 'utf8'), status);

const base = {
  capture_active: true, active_profile: 'profile-a', selected_source: 'Game',
  analysis_fps: 10, input_fps: 60, last_analysis_age_ms: 10,
  detector_errors: 0, replaced_frames: 0
};
const view = (patch, elapsed = 0) => status.presentation({ ...base, ...patch }, 'My game', elapsed, '');

function harness() {
  let disconnected = 0;
  const sent = [];
  const client = status.createClient({
    send: line => sent.push(JSON.parse(line)), changed: () => {},
    disconnect: () => disconnected++
  });
  const reply = (result, now = 100) => client.receive(JSON.stringify({
    jsonrpc: '2.0', id: sent.at(-1).id, result
  }), now);
  return { client, sent, reply, disconnected: () => disconnected };
}

test('idle, stopped, capture-only, waiting, fresh processing, and stalled', () => {
  assert.equal(view({ capture_active: false, active_profile: null }).label, 'Idle');
  assert.equal(view({ capture_active: false }).label, 'Stopped');
  assert.equal(view({ active_profile: null }).label, 'Capturing');
  assert.equal(view({ last_analysis_age_ms: null }).label, 'Waiting');
  assert.equal(view({}).label, 'Processing');
  assert.equal(view({ last_analysis_age_ms: 6000 }).label, 'Stalled');
  assert.equal(view({}, 6000).label, 'Stalled');
  assert.equal(status.presentation(null, '', 0, 'gone').label, 'Offline');
});

test('historical FPS and older daemons never prove current processing', () => {
  assert.equal(view({ last_analysis_age_ms: undefined }).label, 'Capturing');
  for (const age of [-1, '0', Infinity, NaN])
    assert.equal(view({ last_analysis_age_ms: age }).label, 'Stalled');
  assert.equal(view({ last_analysis_age_ms: 5000 }, 1).label, 'Stalled');
});

test('stopped capture hides the selected profile, including during output errors', () => {
  for (const patch of [{ capture_active: false }, { capture_active: false, output_error: 'disk full' }]) {
    const stopped = view(patch);
    assert.equal(stopped.profile, 'None');
    assert.doesNotMatch(stopped.tooltip, /Profile:|My game|profile-a/);
  }
  assert.equal(view({}).profile, 'My game');
  assert.match(view({}).tooltip, /Profile: My game/);
});

test('stopping clears the profile cache and restarting looks up the same stable ID', () => {
  const h = harness();
  h.client.connected(0);
  h.reply({ protocol: 1 });
  h.reply({ ...base, capture_active: false });
  assert.equal(h.sent.length, 2); // no profile lookup while stopped
  assert.equal(h.client.view(100).profile, 'None');
  h.client.poll(200);
  h.reply(base, 200);
  assert.equal(h.sent.at(-1).method, 'profile.get');
  h.reply({ id: 'profile-a', name: 'My game' }, 200);
  h.client.poll(300);
  h.reply({ ...base, capture_active: false }, 300);
  assert.equal(h.client.profileName, '');
  assert.equal(h.client.profileId, null);
  h.client.poll(400);
  h.reply(base, 400);
  assert.equal(h.sent.at(-1).method, 'profile.get');
});

test('capture and output errors alarm; cumulative detector errors remain diagnostic', () => {
  const error = view({ capture_error: 'source gone', output_error: 'disk full' });
  assert.equal(error.label, 'Error');
  assert.equal(error.alarming, true);
  assert.match(error.tooltip, /source gone/);
  assert.match(error.tooltip, /disk full/);
  assert.equal(view({ output_error: 'disk full' }).label, 'Error');
  assert.equal(view({ detector_errors: 20 }).label, 'Processing');
  assert.equal(view({ detector_errors: 20 }).alarming, false);
});

test('untrusted display text is bounded, strips control characters, and stays inert', () => {
  assert.equal(status.displayText('a\n\x00\u202eb'), 'a   b');
  assert.equal(status.displayText('x'.repeat(1000)).length, 240);
  assert.equal(status.displayText('<b>$(touch /tmp/no)</b>'), '<b>$(touch /tmp/no)</b>');
  assert.equal(status.rate(NaN), '?');
});

test('handshake precedes status and only one request can be outstanding', () => {
  const h = harness();
  h.client.poll(0);
  assert.equal(h.sent.length, 0);
  h.client.connected(0);
  for (let i = 0; i < 1000; i++) h.client.poll(i);
  assert.equal(h.sent.length, 1);
  assert.equal(h.sent[0].method, 'system.handshake');
  assert.equal(h.sent[0].params.protocol, 1);
  h.reply({ protocol: 1 });
  assert.equal(h.sent[1].method, 'system.status');
  h.reply(base);
  assert.equal(h.sent[2].method, 'profile.get');
  assert.equal(h.sent[2].params.profile_id, 'profile-a');
  h.reply({ id: 'profile-a', name: 'My game' });
  assert.equal(h.client.view(100).profile, 'My game');
  h.client.poll(200);
  h.reply(base, 200);
  assert.equal(h.sent.length, 4); // cached profile name
});

test('profile changes, explicit refresh, and daemon restart discard cached identity', () => {
  const h = harness();
  h.client.connected(0);
  h.reply({ protocol: 1 });
  h.reply(base);
  h.reply({ id: 'profile-a', name: 'A' });
  h.client.poll(200);
  h.reply({ ...base, active_profile: 'profile-b' }, 200);
  assert.equal(h.client.profileName, '');
  assert.equal(h.sent.at(-1).params.profile_id, 'profile-b');
  h.reply({ id: 'profile-b', name: 'B' }, 200);
  h.client.refresh(300);
  h.reply({ ...base, active_profile: 'profile-b' }, 300);
  assert.equal(h.sent.at(-1).method, 'profile.get');
  h.client.disconnected();
  assert.equal(h.client.view(301).label, 'Offline');
  assert.equal(h.client.profileName, '');
  h.client.connected(400);
  assert.equal(h.sent.at(-1).method, 'system.handshake');
});

test('failed profile lookup retains status and stable ID fallback', () => {
  const h = harness();
  h.client.connected(0);
  h.reply({ protocol: 1 });
  h.reply(base);
  h.client.receive(JSON.stringify({ jsonrpc: '2.0', id: h.sent.at(-1).id,
    error: { code: -32602, message: 'Profile missing' } }), 200);
  assert.equal(h.client.view(200).label, 'Processing');
  assert.equal(h.client.view(200).profile, 'profile-a');
  assert.equal(h.disconnected(), 0);
});

test('timeout clears stale status and releases pending request', () => {
  const h = harness();
  h.client.connected(0);
  h.reply({ protocol: 1 }, 0);
  h.reply(base, 0);
  h.client.tick(5000);
  assert.equal(h.client.view(5000).label, 'Offline');
  assert.equal(h.client.pending, null);
  assert.equal(h.disconnected(), 1);
  assert.match(h.client.view(5000).tooltip, /timed out/);
});

test('malformed, mismatched, oversized, invalid, and incompatible replies disconnect', () => {
  for (const reply of [
    '{', 'null', '{}', 'x'.repeat(1024 * 1024 + 1),
    JSON.stringify({ jsonrpc: '2.0', id: 99, result: {} }),
    JSON.stringify({ jsonrpc: '2.0', id: 1, result: { protocol: 2 } }),
    JSON.stringify({ jsonrpc: '2.0', id: 1, result: {}, error: {} }),
    JSON.stringify({ jsonrpc: '2.0', id: 1, error: { message: 'Rejected' } })
  ]) {
    const h = harness();
    h.client.connected(0);
    h.client.receive(reply, 1);
    assert.equal(h.disconnected(), 1);
    assert.equal(h.client.ready, false);
  }
  const h = harness();
  h.client.connected(0);
  h.reply({ protocol: 1 });
  h.reply({ capture_active: 'yes', active_profile: null });
  assert.equal(h.disconnected(), 1);
});
