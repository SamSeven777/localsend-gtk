'use strict';

// Exercise the shipped browser-download script with browser boundaries mocked.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');
const html = fs.readFileSync(path.join(__dirname, '..', 'assets', 'web-share.html'), 'utf8');
const scripts = [...html.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/gi)];
assert.equal(scripts.length, 1);
const source = scripts[0][1];
const base = '/private-capability/';
const tick = () => new Promise(resolve => setImmediate(resolve));

function browser() {
  const downloads = [], timers = [], requests = [], withdrawals = [], beacons = [];
  const listeners = new Map();
  const element = tag => {
    const classes = new Set();
    return {
      tag, children: [], textContent: '', disabled: false, hidden: false,
      append(...children) { this.children.push(...children); },
      replaceChildren(...children) { this.children = children; },
      classList: { toggle(name, enabled) { if (enabled) classes.add(name); else classes.delete(name); } },
      click() { downloads.push(this); },
      set innerHTML(_) { throw Error('Shared file names must never be injected as HTML'); },
    };
  };
  const elements = Object.fromEntries(['status', 'files', 'all', 'retry', 'disconnect'].map(id => [id, element(id)]));
  elements.all.hidden = true;
  elements.retry.hidden = true;
  let random = 0;
  function fetch(url, options) {
    if (url.startsWith(base + 'withdraw/')) { withdrawals.push({ url, options }); return Promise.resolve({ ok: true, status: 204 }); }
    assert.equal(url, base + 'prepare');
    assert.equal(options.method, 'POST');
    let complete, fail;
    const promise = new Promise((resolve, reject) => { complete = resolve; fail = reject; });
    let settled = false;
    const request = {
      url, options, body: JSON.parse(options.body), aborted: false,
      respond(manifest, status = 200, deferredJson = false) {
        assert.equal(settled, false);
        settled = true;
        options.signal.removeEventListener('abort', abort);
        let json;
        if (deferredJson) json = new Promise(resolve => { request.finishJson = () => resolve(manifest); });
        complete({ ok: status >= 200 && status < 300, status, json: () => deferredJson ? json : Promise.resolve(manifest) });
      },
      fail() { settled = true; options.signal.removeEventListener('abort', abort); fail(Error('Offline')); },
    };
    function abort() {
      if (settled) return;
      settled = true;
      request.aborted = true;
      const error = Error('Aborted'); error.name = 'AbortError'; fail(error);
    }
    options.signal.addEventListener('abort', abort, { once: true });
    requests.push(request);
    return promise;
  }
  const context = vm.createContext({
    document: { getElementById: id => elements[id], createElement: element },
    location: { pathname: base },
    crypto: { getRandomValues(bytes) { bytes.fill(++random); return bytes; } },
    fetch, AbortController, Uint8Array, encodeURIComponent,
    navigator: { sendBeacon(url) { beacons.push(url); return true; } },
    window: { addEventListener(name, callback) { listeners.set(name, callback); } },
    setTimeout(callback) { timers.push(callback); return timers.length; },
  });
  vm.runInContext(source, context, { filename: 'web-share.html' });
  return { elements, downloads, timers, requests, withdrawals, beacons, listeners };
}
const manifest = {
  session: 'approved-session',
  files: [
    { id: 'one', name: 'folder/青い <img src=x>.txt', size: 0 },
    { id: 'two', name: 'Message.txt', size: 2048 },
  ],
};

test('requests approval before exposing safe file links and preserves Unicode', async () => {
  const page = browser();
  assert.equal(page.requests.length, 1);
  assert.match(page.requests[0].body.requestId, /^[0-9a-f]{32}$/);
  assert.equal(page.elements.files.children.length, 0);
  page.requests[0].respond(manifest);
  await tick();
  const links = page.elements.files.children;
  assert.equal(links.length, 2);
  assert.equal(links[0].children[1].textContent, manifest.files[0].name);
  assert.equal(links[0].download, '青い <img src=x>.txt');
  assert.equal(links[0].href, base + 'file/approved-session/one');
  assert.equal(links[0].children[2].textContent, '0 B');
  assert.equal(links[1].children[2].textContent, '2.0 KB');
  assert.equal(page.elements.status.textContent, 'Files (2)');
  assert.equal(page.elements.all.hidden, false);
});

test('decline leaves no file access and retry gets a new cancellation identifier', async () => {
  const page = browser();
  const original = page.requests[0].body.requestId;
  page.requests[0].respond({}, 403);
  await tick();
  assert.match(page.elements.status.textContent, /declined/);
  assert.equal(page.elements.retry.hidden, false);
  assert.equal(page.elements.files.children.length, 0);
  assert.equal(page.withdrawals[0].url, base + 'withdraw/' + original);
  page.elements.retry.onclick();
  assert.equal(page.requests.length, 2);
  assert.notEqual(page.requests[1].body.requestId, original);
  assert.equal(page.elements.retry.hidden, true);
});

test('cancel withdraws pending native consent and does not revive a stale response', async () => {
  const page = browser();
  page.requests[0].respond(manifest, 200, true);
  await tick();
  page.elements.disconnect.onclick();
  page.requests[0].finishJson();
  await tick();
  assert.equal(page.withdrawals.length, 1);
  assert.equal(page.elements.files.children.length, 0);
  assert.equal(page.elements.retry.hidden, false);
  assert.match(page.elements.status.textContent, /Disconnected/);
  const pending = browser();
  pending.elements.disconnect.onclick();
  await tick();
  assert.equal(pending.requests[0].aborted, true);
  assert.equal(pending.withdrawals.length, 1);
});

test('download all starts native browser downloads sequentially without buffering blobs', async () => {
  const page = browser();
  page.requests[0].respond(manifest);
  await tick();
  const sending = page.elements.all.onclick();
  assert.equal(page.downloads.length, 1);
  assert.equal(page.elements.all.disabled, true);
  page.timers.shift()();
  await tick();
  assert.equal(page.downloads.length, 2);
  page.timers.shift()();
  await sending;
  assert.equal(page.elements.all.disabled, false);
  assert.match(page.elements.status.textContent, /Downloads started/);
  assert.equal(page.requests.length, 1, 'File payloads go to native download links, never fetch-to-blob');
});

test('disconnect stops a download-all sequence and revokes active browser access', async () => {
  const page = browser();
  page.requests[0].respond(manifest);
  await tick();
  const sending = page.elements.all.onclick();
  page.elements.disconnect.onclick();
  page.timers.shift()();
  await sending;
  assert.equal(page.downloads.length, 1);
  assert.equal(page.withdrawals.length, 1);
  assert.equal(page.elements.files.children.length, 0);
  assert.match(page.elements.status.textContent, /Disconnected/);
});

test('closing the page withdraws both pending and accepted browser sessions', async () => {
  const page = browser();
  page.listeners.get('pagehide')();
  assert.equal(page.beacons[0], base + 'withdraw/' + page.requests[0].body.requestId);
  page.requests[0].respond(manifest);
  await tick();
  page.listeners.get('pagehide')();
  assert.equal(page.beacons.length, 2);
  page.elements.disconnect.onclick();
  page.listeners.get('pagehide')();
  assert.equal(page.beacons.length, 2);
});
