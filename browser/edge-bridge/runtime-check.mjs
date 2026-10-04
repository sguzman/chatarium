import assert from 'node:assert/strict';
import fs from 'node:fs';

const root = new URL('./', import.meta.url);
const fixture = fs.readFileSync(
  new URL('known-good-discovery-v0.3.txt', root),
  'utf8',
).replaceAll('\r\n', '\n');
const worker = fs.readFileSync(
  new URL('service-worker.js', root),
  'utf8',
).replaceAll('\r\n', '\n');

function fixtureSection(text, startMarker, endMarker) {
  const start = text.indexOf(startMarker);
  const end = text.indexOf(endMarker, start + startMarker.length);
  if (start < 0 || end < 0) throw new Error(`missing fixture section ${startMarker}`);
  let section = text.slice(start + startMarker.length, end);
  if (section.startsWith('\n')) section = section.slice(1);
  if (section.endsWith('\n')) section = section.slice(0, -1);
  return section;
}

const frozenDiscoveryCommand = fixtureSection(
  fixture,
  '--- DISCOVERY COMMAND ---',
  '--- END DISCOVERY COMMAND ---',
);

function extractAsyncFunction(source, name) {
  const marker = `async function ${name}(`;
  const starts = [];
  let from = 0;
  while (true) {
    const index = source.indexOf(marker, from);
    if (index < 0) break;
    starts.push(index);
    from = index + marker.length;
  }
  assert.equal(
    starts.length,
    1,
    `expected exactly one ${name} definition in service-worker.js`,
  );

  const start = starts[0];
  const open = source.indexOf('{', start);
  let depth = 0;
  let quote = null;
  let escaped = false;

  for (let index = open; index < source.length; index += 1) {
    const char = source[index];
    if (quote !== null) {
      if (escaped) {
        escaped = false;
        continue;
      }
      if (char === '\\') {
        escaped = true;
        continue;
      }
      if (char === quote) quote = null;
      continue;
    }
    if (char === "'" || char === '"' || char === '`') {
      quote = char;
      continue;
    }
    if (char === '{') depth += 1;
    if (char === '}') {
      depth -= 1;
      if (depth === 0) return source.slice(start, index + 1);
    }
  }

  throw new Error(`unterminated async function ${name}`);
}

const discoveryCommand = extractAsyncFunction(worker, 'discoverHistorySurfaces');
assert.equal(
  discoveryCommand,
  frozenDiscoveryCommand,
  'active discoverHistorySurfaces must exactly equal the live-proven 0.3 fixture',
);

const freshDiscoveryCommand = extractAsyncFunction(
  worker,
  'discoverHistorySurfacesFreshTab',
);
assert.ok(
  !freshDiscoveryCommand.includes('/backend-api/'),
  'fresh-tab recovery must not construct private backend URLs',
);
assert.ok(
  !freshDiscoveryCommand.includes('fetch('),
  'fresh-tab recovery must not synthesize HTTP requests',
);
assert.ok(
  !freshDiscoveryCommand.includes('Network.setCacheDisabled'),
  'fresh-tab recovery must not mutate cache policy',
);

function harness(options = {}) {
  const activeDiscoveries = new Map();
  const calls = [];
  const sleeps = [];

  const chrome = {
    debugger: {
      async attach(debuggee, version) {
        calls.push(['attach', debuggee.tabId, version]);
        if (options.attachError) throw new Error(options.attachError);
      },
      async sendCommand(debuggee, method, args) {
        calls.push(['sendCommand', debuggee.tabId, method, args ?? null]);
        if (options.sendCommandError === method) {
          throw new Error(`forced_${method}_failure`);
        }
      },
      async detach(debuggee) {
        calls.push(['detach', debuggee.tabId]);
        if (options.detachError) throw new Error(options.detachError);
      },
    },
    tabs: {
      async reload(tabId) {
        calls.push(['reload', tabId]);
        const session = activeDiscoveries.get(tabId);
        if (!session) throw new Error('reload without active discovery session');
        if (options.populate !== false) {
          session.accountId = 'account-test';
          session.responses_seen = 7;
          session.backend_200_seen = 3;
          session.json_candidates_seen = 1;
          session.candidates.push({
            path: '/backend-api/gizmos/snorlax/sidebar',
            items: [{ id: 'conversation-1', title: 'One' }],
          });
        }
      },
    },
  };

  const deps = {
    DISCOVERY_PROFILE: 'cdp-history-discovery-v1',
    DEBUGGER_PROTOCOL_VERSION: '1.3',
    MAX_RESPONSE_BYTES: 4 * 1024 * 1024,
    DISCOVERY_WINDOW_MS: 8_000,
    DISCOVERY_BODY_GRACE_MS: 500,
    activeDiscoveries,
    chrome,
    baseResult(command, requestProfile) {
      return {
        version: 1,
        id: command.id,
        kind: command.kind,
        ok: false,
        bridge_transport: 'extension-cdp',
        extension_version: 'test',
        chatgpt_tab_found: false,
        debugger_attached: false,
        network_enabled: false,
        reload_started: false,
        account_context: false,
        request_profile: requestProfile,
      };
    },
    async findChatGptTab() {
      return options.noTab ? null : { id: 42 };
    },
    async sleep(ms) {
      sleeps.push(ms);
    },
    async accountContextForTab() {
      calls.push(['accountContextForTab']);
      return options.accountContext ?? null;
    },
    mergeDiscoveryCandidates(candidates) {
      return candidates;
    },
  };

  const factory = new Function(
    'deps',
    `
      const {
        DISCOVERY_PROFILE,
        DEBUGGER_PROTOCOL_VERSION,
        MAX_RESPONSE_BYTES,
        DISCOVERY_WINDOW_MS,
        DISCOVERY_BODY_GRACE_MS,
        activeDiscoveries,
        chrome,
        baseResult,
        findChatGptTab,
        sleep,
        accountContextForTab,
        mergeDiscoveryCandidates,
      } = deps;
      ${discoveryCommand}
      return discoverHistorySurfaces;
    `,
  );

  return {
    run: factory(deps),
    activeDiscoveries,
    calls,
    sleeps,
  };
}

{
  const h = harness();
  const result = await h.run({ id: 'c1', kind: 'discover_history_surfaces' });
  assert.equal(result.ok, true);
  assert.equal(result.discovery, 'candidates_observed');
  assert.equal(result.chatgpt_tab_found, true);
  assert.equal(result.debugger_attached, true);
  assert.equal(result.network_enabled, true);
  assert.equal(result.reload_started, true);
  assert.equal(result.account_context, true);
  assert.equal(result.responses_seen, 7);
  assert.equal(result.backend_http_200_seen, 3);
  assert.equal(result.json_candidates_seen, 1);
  assert.equal(result.candidate_count, 1);
  assert.deepEqual(h.sleeps, [8_000, 500]);
  assert.equal(h.activeDiscoveries.size, 0);
  assert.deepEqual(
    h.calls.filter((call) => ['attach', 'reload', 'detach'].includes(call[0])).map((call) => call[0]),
    ['attach', 'reload', 'detach'],
  );
}

{
  const h = harness({ populate: false });
  const result = await h.run({ id: 'c2', kind: 'discover_history_surfaces' });
  assert.equal(result.ok, false);
  assert.equal(result.error, 'cdp_no_network_responses');
  assert.equal(result.candidate_count, 0);
  assert.equal(h.activeDiscoveries.size, 0);
  assert.ok(h.calls.some((call) => call[0] === 'detach'));
}

{
  const h = harness({ sendCommandError: 'Network.enable' });
  const result = await h.run({ id: 'c3', kind: 'discover_history_surfaces' });
  assert.equal(result.ok, false);
  assert.equal(result.error, 'forced_Network.enable_failure');
  assert.equal(h.activeDiscoveries.size, 0);
  assert.ok(h.calls.some((call) => call[0] === 'detach'));
  assert.ok(!h.calls.some((call) => call[0] === 'reload'));
}

{
  const h = harness();
  h.activeDiscoveries.set(42, { sentinel: true });
  const result = await h.run({ id: 'c4', kind: 'discover_history_surfaces' });
  assert.equal(result.ok, false);
  assert.equal(result.error, 'history_discovery_already_active');
  assert.equal(h.calls.length, 0);
}

{
  const h = harness({ noTab: true });
  const result = await h.run({ id: 'c5', kind: 'discover_history_surfaces' });
  assert.equal(result.ok, false);
  assert.equal(result.error, 'chatgpt_tab_not_found');
  assert.equal(h.calls.length, 0);
}

console.log('Chatarium active frozen 0.3 discovery runtime harness OK');

function freshHarness(options = {}) {
  const activeDiscoveries = new Map();
  const calls = [];
  const sleeps = [];
  let nextTabId = 100;

  const chrome = {
    debugger: {
      async attach(debuggee, version) {
        calls.push(['attach', debuggee.tabId, version]);
        if (options.attachError) throw new Error(options.attachError);
      },
      async sendCommand(debuggee, method, args) {
        calls.push(['sendCommand', debuggee.tabId, method, args ?? null]);
        if (options.sendCommandError === method) {
          throw new Error(`forced_${method}_failure`);
        }
        if (method === 'Page.navigate' && options.populate !== false) {
          const session = activeDiscoveries.get(debuggee.tabId);
          if (!session) throw new Error('navigate without active discovery session');
          session.accountId = 'account-test';
          session.responses_seen = 11;
          session.backend_200_seen = 5;
          session.json_candidates_seen = 2;
          session.candidates.push({
            path: '/backend-api/gizmos/snorlax/sidebar',
            items: [
              { id: 'fresh-1', title: 'Fresh one' },
              { id: 'fresh-2', title: 'Fresh two' },
            ],
          });
        }
      },
      async detach(debuggee) {
        calls.push(['detach', debuggee.tabId]);
        if (options.detachError) throw new Error(options.detachError);
      },
    },
    tabs: {
      async create(args) {
        calls.push(['create', args]);
        if (options.createFailure) return null;
        return { id: nextTabId++, windowId: 7 };
      },
      async remove(tabId) {
        calls.push(['remove', tabId]);
        if (options.removeError) throw new Error(options.removeError);
      },
    },
  };

  const deps = {
    FRESH_DISCOVERY_PROFILE: 'cdp-history-fresh-tab-v1',
    FRESH_DISCOVERY_ROUTE: 'https://chatgpt.com/',
    DEBUGGER_PROTOCOL_VERSION: '1.3',
    MAX_RESPONSE_BYTES: 4 * 1024 * 1024,
    DISCOVERY_WINDOW_MS: 8_000,
    DISCOVERY_BODY_GRACE_MS: 500,
    activeDiscoveries,
    chrome,
    baseResult(command, requestProfile) {
      return {
        version: 1,
        id: command.id,
        kind: command.kind,
        ok: false,
        bridge_transport: 'extension-cdp',
        extension_version: 'test',
        chatgpt_tab_found: false,
        debugger_attached: false,
        network_enabled: false,
        capture_tab_created: false,
        navigation_started: false,
        account_context: false,
        request_profile: requestProfile,
      };
    },
    async findChatGptTab() {
      return options.noTab ? null : { id: 42, windowId: 7 };
    },
    async sleep(ms) {
      sleeps.push(ms);
    },
    async accountContextForTab(tabId) {
      calls.push(['accountContextForTab', tabId]);
      return options.accountContext ?? 'account-source';
    },
    mergeDiscoveryCandidates(candidates) {
      return candidates;
    },
  };

  const factory = new Function(
    'deps',
    `
      const {
        FRESH_DISCOVERY_PROFILE,
        FRESH_DISCOVERY_ROUTE,
        DEBUGGER_PROTOCOL_VERSION,
        MAX_RESPONSE_BYTES,
        DISCOVERY_WINDOW_MS,
        DISCOVERY_BODY_GRACE_MS,
        activeDiscoveries,
        chrome,
        baseResult,
        findChatGptTab,
        sleep,
        accountContextForTab,
        mergeDiscoveryCandidates,
      } = deps;
      ${freshDiscoveryCommand}
      return discoverHistorySurfacesFreshTab;
    `,
  );

  return {
    run: factory(deps),
    activeDiscoveries,
    calls,
    sleeps,
  };
}

{
  const h = freshHarness();
  const result = await h.run({
    id: 'f1',
    kind: 'discover_history_surfaces_fresh_tab',
  });
  assert.equal(result.ok, true);
  assert.equal(result.discovery, 'candidates_observed');
  assert.equal(result.chatgpt_tab_found, true);
  assert.equal(result.capture_tab_created, true);
  assert.equal(result.debugger_attached, true);
  assert.equal(result.network_enabled, true);
  assert.equal(result.navigation_started, true);
  assert.equal(result.account_context, true);
  assert.equal(result.responses_seen, 11);
  assert.equal(result.backend_http_200_seen, 5);
  assert.equal(result.json_candidates_seen, 2);
  assert.equal(result.candidate_count, 1);
  assert.deepEqual(h.sleeps, [8_000, 500]);
  assert.equal(h.activeDiscoveries.size, 0);
  assert.deepEqual(
    h.calls
      .filter((call) => ['create', 'attach', 'detach', 'remove'].includes(call[0]))
      .map((call) => call[0]),
    ['create', 'attach', 'detach', 'remove'],
  );
  const navigate = h.calls.find(
    (call) => call[0] === 'sendCommand' && call[2] === 'Page.navigate',
  );
  assert.equal(navigate[3].url, 'https://chatgpt.com/');
}

{
  const h = freshHarness({ populate: false });
  const result = await h.run({
    id: 'f2',
    kind: 'discover_history_surfaces_fresh_tab',
  });
  assert.equal(result.ok, false);
  assert.equal(result.error, 'cdp_fresh_history_no_network_responses');
  assert.equal(h.activeDiscoveries.size, 0);
  assert.ok(h.calls.some((call) => call[0] === 'detach'));
  assert.ok(h.calls.some((call) => call[0] === 'remove'));
}

{
  const h = freshHarness({ sendCommandError: 'Network.enable' });
  const result = await h.run({
    id: 'f3',
    kind: 'discover_history_surfaces_fresh_tab',
  });
  assert.equal(result.ok, false);
  assert.equal(result.error, 'forced_Network.enable_failure');
  assert.equal(h.activeDiscoveries.size, 0);
  assert.ok(h.calls.some((call) => call[0] === 'detach'));
  assert.ok(h.calls.some((call) => call[0] === 'remove'));
  assert.ok(
    !h.calls.some(
      (call) => call[0] === 'sendCommand' && call[2] === 'Page.navigate',
    ),
  );
}

{
  const h = freshHarness({ createFailure: true });
  const result = await h.run({
    id: 'f4',
    kind: 'discover_history_surfaces_fresh_tab',
  });
  assert.equal(result.ok, false);
  assert.equal(result.error, 'fresh_history_tab_creation_failed');
  assert.equal(h.activeDiscoveries.size, 0);
  assert.ok(!h.calls.some((call) => call[0] === 'attach'));
}

{
  const h = freshHarness({ noTab: true });
  const result = await h.run({
    id: 'f5',
    kind: 'discover_history_surfaces_fresh_tab',
  });
  assert.equal(result.ok, false);
  assert.equal(result.error, 'chatgpt_tab_not_found');
  assert.equal(h.calls.length, 0);
}

console.log('Chatarium isolated fresh-tab discovery runtime harness OK');

