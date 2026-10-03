const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const vm = require('node:vm');
const appPath = process.argv[2] || require('node:path').join(__dirname, 'webui/lifecycle.js');
const handlers = {};
const documentHandlers = {};
const timers = new Map();
const requests = [];
let timerId = 0;
let tab = 0;
let failHeartbeat = false;
let heldHeartbeat = null;
const context = vm.createContext({
  window: { addEventListener: (type, handler) => { handlers[type] = handler; } },
  document: { addEventListener: (type, handler) => { documentHandlers[type] = handler; } },
  crypto: { randomUUID: () => `tab-${++tab}` },
  AbortSignal: { timeout: () => undefined },
  setTimeout: (handler, delay) => { timers.set(++timerId, { handler, delay }); return timerId; },
  clearTimeout: (id) => timers.delete(id),
  fetch: async (path, options) => {
    requests.push({ path, options });
    if (heldHeartbeat && path.endsWith('heartbeat')) await heldHeartbeat;
    if (failHeartbeat && path.endsWith('heartbeat')) throw new Error('offline');
    return { ok: true };
  },
});
const settle = () => new Promise(resolve => setImmediate(resolve));
(async () => {
  vm.runInContext(readFileSync(appPath, 'utf8'), context);
  const life = context.window.AccountManagerLifecycle;
  assert.ok(life, 'Lifecycle module must exist');
  life.connect('fixture-token');
  await settle();
  assert.equal(requests[0].path, '/api/heartbeat');
  assert.equal(requests[0].options.headers['x-codex-pool-token'], 'fixture-token');
  assert.equal(timers.size, 1);
  life.connect('fixture-token');
  await settle();
  assert.equal(requests.length, 1, 'Polling must not create concurrent lease loops');
  documentHandlers.visibilitychange();
  await settle();
  assert.equal(requests.length, 2, 'Hidden tabs renew independently of inventory polling');
  assert.equal(timers.size, 1);
  handlers.pagehide({ persisted: true });
  await settle();
  assert.equal(requests.at(-1).path, '/api/leave');
  assert.equal(requests.at(-1).options.keepalive, true);
  assert.equal(timers.size, 0);
  const oldId = JSON.parse(requests.at(-1).options.body).clientId;
  handlers.pageshow({ persisted: true });
  await settle();
  assert.notEqual(JSON.parse(requests.at(-1).options.body).clientId, oldId, 'Back-forward cache restores a fresh lease');
  assert.equal(timers.size, 1);
  failHeartbeat = true;
  documentHandlers.visibilitychange();
  await settle();
  assert.equal(timers.size, 1, 'A temporary network failure retries without spinning');
  failHeartbeat = false;
  let releaseHeartbeat;
  heldHeartbeat = new Promise(resolve => { releaseHeartbeat = resolve; });
  documentHandlers.visibilitychange();
  await settle();
  handlers.pagehide({ persisted: true });
  handlers.pageshow({ persisted: true });
  releaseHeartbeat();
  heldHeartbeat = null;
  await settle();
  assert.equal(timers.size, 1, 'Pending old-page heartbeats must not lose the restored lease loop');
  life.stop();
  assert.equal(timers.size, 0);
  const requestCount = requests.length;
  life.connect('fixture-token');
  handlers.pageshow({ persisted: true });
  documentHandlers.visibilitychange();
  await settle();
  assert.equal(requests.length, requestCount, 'Stopped panels must stay stopped');
  console.log('WebUI lifecycle browser harness: passed');
})().catch((error) => { console.error(error); process.exitCode = 1; });
