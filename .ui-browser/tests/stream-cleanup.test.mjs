import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import https from 'node:https';
import { before, test } from 'node:test';
import { setTimeout as sleep } from 'node:timers/promises';

const origin = 'https://app:4623';
const ca = readFileSync('/opt/ui-browser/tls/ca.crt');

function get(path) {
  return new Promise((resolve, reject) => {
    const req = https.get(`${origin}${path}`, { ca, rejectUnauthorized: true, agent: false }, (res) => {
      req.setTimeout(0);
      resolve(res);
    });
    req.setTimeout(3000, () => req.destroy(new Error(`request timed out: ${path}`)));
    req.on('error', reject);
  });
}

async function state() {
  const res = await get('/_fixture/state');
  assert.equal(res.statusCode, 200);
  let body = '';
  for await (const chunk of res) body += chunk;
  return JSON.parse(body);
}

before(async () => {
  const deadline = Date.now() + 5000;
  let lastError;
  do {
    try {
      await state();
      return;
    } catch (error) {
      lastError = error;
      await sleep(100);
    }
  } while (Date.now() < deadline);
  throw lastError;
});

test('client disconnect closes the backend SSE stream', { timeout: 10000 }, async () => {
  const res = await get('/api/translator/console-logs/stream');
  try {
    assert.equal(res.statusCode, 200);
    assert.match(res.headers['content-type'], /^text\/event-stream/);
    await new Promise((resolve, reject) => {
      let data = '';
      const timer = setTimeout(() => reject(new Error('SSE message timed out')), 5000);
      res.on('data', (chunk) => {
        data += chunk;
        if (data.includes('data: ready\n\n')) { clearTimeout(timer); resolve(); }
      });
      res.once('error', (error) => { clearTimeout(timer); reject(error); });
      res.once('end', () => { clearTimeout(timer); reject(new Error('SSE ended before message')); });
    });
    assert.equal((await state()).clientStream.closed, false, 'backend must remain open before disconnect');
    res.destroy();

    const deadline = Date.now() + 5000;
    let snapshot;
    do {
      snapshot = await state();
      if (snapshot.clientStream.closed) break;
      await sleep(50);
    } while (Date.now() < deadline);
    assert.equal(snapshot.clientStream.closed, true, 'backend did not observe a premature close');
  } finally {
    res.destroy();
  }
});

test('backend abort closes the downstream SSE response', { timeout: 10000 }, async () => {
  const res = await get('/_fixture/abort');
  try {
    assert.equal(res.statusCode, 200);
    assert.match(res.headers['content-type'], /^text\/event-stream/);
    const result = await new Promise((resolve, reject) => {
      let data = '';
      let aborted = false;
      const timer = setTimeout(() => {
        reject(new Error('downstream response did not close after backend abort'));
        res.destroy();
      }, 5000);
      res.on('data', (chunk) => { data += chunk; });
      res.on('aborted', () => { aborted = true; });
      res.on('error', () => {});
      res.on('close', () => {
        clearTimeout(timer);
        resolve({ data, aborted, complete: res.complete });
      });
    });
    assert.match(result.data, /data: ready/);
    assert.equal(result.aborted, true);
    assert.equal(result.complete, false);
    assert.equal((await state()).aborted, true, 'backend must have aborted the response');
  } finally {
    res.destroy();
  }
});
