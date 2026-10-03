'use strict';

// Run the shipped page script with browser boundaries mocked. These tests exercise
// the real file-input/cancel handlers, never a second implementation of send().
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const pagePath = path.join(__dirname, '..', 'assets', 'web-receive.html');
const html = fs.readFileSync(pagePath, 'utf8');
const inlineScripts = [...html.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/gi)];
assert.equal(inlineScripts.length, 1, 'The upload page must contain one inline script');
const source = inlineScripts[0][1];
const base = '/test-capability/';
const tick = () => new Promise(resolve => setImmediate(resolve));

function browser() {
  const elements = Object.fromEntries(
    ['choose', 'files', 'drop', 'status', 'progress', 'cancel'].map(id => {
      const classes = new Set();
      return [id, {
        disabled: false,
        hidden: ['progress', 'cancel'].includes(id),
        textContent: '',
        value: id === 'progress' ? 0 : '',
        files: [],
        clicks: 0,
        click() { this.clicks += 1; },
        classList: {
          add(value) { classes.add(value); },
          remove(value) { classes.delete(value); },
          contains(value) { return classes.has(value); },
        },
      }];
    }),
  );
  const offers = [];
  const withdrawals = [];
  const uploads = [];
  const beacons = [];
  const listeners = new Map();
  let randomByte = 0;

  function fetch(url, options) {
    if (url.startsWith(`${base}withdraw/`)) {
      withdrawals.push({ url, options });
      return Promise.resolve({ ok: true, status: 204 });
    }
    assert.equal(url, `${base}offer`, 'Unexpected browser request');
    assert.equal(options.method, 'POST');
    assert.equal(options.headers['Content-Type'], 'application/json');
    const body = JSON.parse(options.body);
    let resolve;
    let reject;
    let settled = false;
    const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
    const offer = {
      url, options, body, aborted: false,
      respond(approval, status = 200) {
        assert.equal(settled, false, 'Offer has already finished');
        settled = true;
        options.signal.removeEventListener('abort', onAbort);
        resolve({ ok: status >= 200 && status < 300, status, json: async () => approval });
      },
      fail(message = 'Network unavailable') {
        assert.equal(settled, false, 'Offer has already finished');
        settled = true;
        options.signal.removeEventListener('abort', onAbort);
        reject(new Error(message));
      },
    };
    function onAbort() {
      if (settled) return;
      settled = true;
      offer.aborted = true;
      const error = new Error('The operation was aborted');
      error.name = 'AbortError';
      reject(error);
    }
    options.signal.addEventListener('abort', onAbort, { once: true });
    if (options.signal.aborted) onAbort();
    offers.push(offer);
    return promise;
  }

  class XMLHttpRequest {
    constructor() {
      this.upload = {};
      this.status = 0;
      this.ended = false;
      this.aborted = false;
      uploads.push(this);
    }
    open(method, url) { this.method = method; this.url = url; }
    send(file) { this.file = file; }
    abort() {
      // Browser XHR does not reject an already completed upload a second time.
      if (this.ended) return;
      this.aborted = true;
      this.ended = true;
      this.onabort?.();
    }
    progress(loaded, total = this.file.size) {
      assert.equal(this.ended, false);
      this.upload.onprogress?.({ lengthComputable: true, loaded, total });
    }
    complete(status = 204) {
      assert.equal(this.ended, false);
      this.status = status;
      this.ended = true;
      this.onload?.();
    }
    fail() {
      assert.equal(this.ended, false);
      this.ended = true;
      this.onerror?.();
    }
  }

  const context = vm.createContext({
    document: { getElementById(id) { assert.ok(elements[id], `Unknown element: ${id}`); return elements[id]; } },
    location: { pathname: base },
    window: { addEventListener(type, handler) { listeners.set(type, handler); } },
    navigator: { sendBeacon(url) { beacons.push(url); return true; } },
    crypto: { getRandomValues(bytes) { for (let i = 0; i < bytes.length; i += 1) bytes[i] = randomByte++ % 256; return bytes; } },
    AbortController,
    XMLHttpRequest,
    fetch,
  });
  vm.runInContext(source, context, { filename: pagePath });
  return {
    elements, offers, withdrawals, uploads, beacons,
    select(files) {
      elements.files.files = files;
      elements.files.value = 'C:\\fakepath\\selection';
      return elements.files.onchange();
    },
    cancel() { elements.cancel.onclick(); },
    pagehide() { listeners.get('pagehide')(); },
  };
}

function idle(browser) {
  assert.equal(browser.elements.choose.disabled, false, 'File selection must become available again');
  assert.equal(browser.elements.cancel.hidden, true, 'Cancel must disappear when the operation ends');
  assert.equal(browser.elements.files.value, '', 'The same files must be selectable for a retry');
}

function withdrew(browser, offer) {
  assert.match(offer.body.requestId, /^[a-f0-9]{32}$/);
  assert.ok(browser.withdrawals.length > 0, 'The receiver must be told to release the pending offer/session');
  for (const request of browser.withdrawals) {
    assert.equal(request.url, `${base}withdraw/${offer.body.requestId}`);
    assert.equal(request.options.method, 'POST');
    assert.equal(request.options.keepalive, true);
  }
}

test('upload only the accepted indices and measure progress against accepted bytes', { timeout: 2000 }, async () => {
  const page = browser();
  const files = [{ name: 'first.txt', size: 4 }, { name: 'skipped.txt', size: 6 }, { name: 'last.txt', size: 8 }];
  const transfer = page.select(files);
  assert.equal(page.elements.choose.disabled, true);
  assert.deepEqual(page.offers[0].body.files, files);
  page.offers[0].respond({ token: 'approved-token', files: ['2', '0'] });
  await tick();
  assert.equal(page.uploads.length, 1);
  assert.equal(page.uploads[0].url, `${base}upload/approved-token/2`);
  assert.equal(page.uploads[0].method, 'POST');
  assert.equal(page.uploads[0].file, files[2]);
  page.uploads[0].progress(4);
  assert.equal(page.elements.progress.value, 4 / 12);
  page.uploads[0].complete();
  await tick();
  assert.equal(page.uploads.length, 2);
  assert.equal(page.uploads[1].url, `${base}upload/approved-token/0`);
  assert.equal(page.uploads[1].file, files[0]);
  page.uploads[1].progress(2);
  assert.equal(page.elements.progress.value, 10 / 12);
  page.uploads[1].complete();
  await transfer;
  assert.equal(page.elements.progress.value, 1);
  assert.equal(page.elements.status.textContent, '2 of 3 files sent. The receiver skipped the others.');
  assert.equal(page.withdrawals.length, 0);
  idle(page);
});

test('cancel while waiting withdraws the request ID and permits a fresh offer', { timeout: 2000 }, async () => {
  const page = browser();
  const files = [{ name: 'waiting.txt', size: 4 }];
  const transfer = page.select(files);
  const original = page.offers[0];
  page.cancel();
  await transfer;
  assert.equal(original.aborted, true);
  withdrew(page, original);
  assert.equal(page.uploads.length, 0);
  assert.equal(page.elements.status.textContent, 'Transfer cancelled.');
  idle(page);
  const retry = page.select(files);
  assert.equal(page.offers.length, 2, 'Cancel must reset the internal busy state');
  assert.notEqual(page.offers[1].body.requestId, original.body.requestId);
  page.offers[1].respond({}, 403);
  await retry;
  idle(page);
});

test('cancel during an upload aborts XHR and never starts remaining files', { timeout: 2000 }, async () => {
  const page = browser();
  const transfer = page.select([{ name: 'first.txt', size: 4 }, { name: 'later.txt', size: 5 }]);
  page.offers[0].respond({ token: 'approved-token', files: ['0', '1'] });
  await tick();
  assert.equal(page.uploads.length, 1);
  page.uploads[0].progress(2);
  page.cancel();
  await transfer;
  await tick();
  assert.equal(page.uploads[0].aborted, true);
  assert.equal(page.uploads.length, 1, 'A canceled transfer must not advance to the next file');
  withdrew(page, page.offers[0]);
  assert.equal(page.elements.status.textContent, 'Transfer cancelled.');
  idle(page);
});

test('offer rejection and fetch failure release busy state and allow a successful retry', { timeout: 2000 }, async () => {
  const page = browser();
  const files = [{ name: 'retry.txt', size: 4 }];
  let transfer = page.select(files);
  page.offers[0].respond({}, 409);
  await transfer;
  assert.match(page.elements.status.textContent, /receiver is busy/);
  idle(page);
  transfer = page.select(files);
  page.offers[1].fail();
  await transfer;
  assert.match(page.elements.status.textContent, /Network unavailable/);
  idle(page);
  transfer = page.select(files);
  assert.equal(page.offers.length, 3);
  page.offers[2].respond({ token: 'retry-token', files: ['0'] });
  await tick();
  page.uploads[0].complete();
  await transfer;
  assert.equal(page.elements.status.textContent, 'All files sent successfully.');
  idle(page);
});

test('XHR failure withdraws the session, skips later files, and allows retry', { timeout: 2000 }, async () => {
  const page = browser();
  const files = [{ name: 'first.txt', size: 4 }, { name: 'later.txt', size: 5 }];
  let transfer = page.select(files);
  page.offers[0].respond({ token: 'first-token', files: ['0', '1'] });
  await tick();
  page.uploads[0].fail();
  await transfer;
  assert.equal(page.uploads.length, 1);
  assert.match(page.elements.status.textContent, /Connection lost/);
  withdrew(page, page.offers[0]);
  idle(page);
  transfer = page.select(files);
  page.offers[1].respond({ token: 'retry-token', files: ['1'] });
  await tick();
  assert.equal(page.uploads[1].url, `${base}upload/retry-token/1`);
  page.uploads[1].complete();
  await transfer;
  assert.match(page.elements.status.textContent, /^1 of 2 files sent/);
  idle(page);
});

test('HTTP upload errors do not report success or continue to later files', { timeout: 2000 }, async () => {
  const page = browser();
  const transfer = page.select([{ name: 'first.txt', size: 4 }, { name: 'later.txt', size: 5 }]);
  page.offers[0].respond({ token: 'approved-token', files: ['0', '1'] });
  await tick();
  page.uploads[0].complete(410);
  await transfer;
  assert.equal(page.uploads.length, 1);
  assert.equal(page.elements.status.textContent, 'The receiver stopped this link.');
  withdrew(page, page.offers[0]);
  idle(page);
});

test('pagehide withdraws a pending offer and completed transfers leave no beacon', { timeout: 2000 }, async () => {
  const page = browser();
  const transfer = page.select([{ name: 'file.txt', size: 4 }]);
  page.pagehide();
  assert.deepEqual(page.beacons, [`${base}withdraw/${page.offers[0].body.requestId}`]);
  page.offers[0].respond({ token: 'approved-token', files: ['0'] });
  await tick();
  page.uploads[0].complete();
  await transfer;
  page.pagehide();
  assert.equal(page.beacons.length, 1, 'A finished transfer must not withdraw a later session');
});

test('ignore overlapping file selections and reject too many files before offering', { timeout: 2000 }, async () => {
  const page = browser();
  await page.select(Array.from({ length: 513 }, (_, index) => ({ name: `${index}.txt`, size: 1 })));
  assert.equal(page.offers.length, 0);
  assert.equal(page.elements.choose.disabled, false);
  assert.match(page.elements.status.textContent, /up to 512 files/);
  const transfer = page.select([{ name: 'first.txt', size: 4 }]);
  await page.select([{ name: 'second.txt', size: 4 }]);
  assert.equal(page.offers.length, 1, 'Busy state must prevent a competing offer');
  page.cancel();
  await transfer;
  idle(page);
});
