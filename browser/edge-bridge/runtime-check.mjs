import assert from 'node:assert/strict';
import fs from 'node:fs';

const root = new URL('./', import.meta.url);
const fixture = fs.readFileSync(
  new URL('known-good-discovery-v0.3.txt', root),
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

const discoveryCommand = fixtureSection(
  fixture,
  '--- DISCOVERY COMMAND ---',
  '--- END DISCOVERY COMMAND ---',
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

console.log('Chatarium frozen 0.3 discovery runtime harness OK');
