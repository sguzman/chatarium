import fs from 'node:fs';
import {
  classifyHistoryBody,
  isCandidateResponse,
  mergeDiscoveryCandidates,
} from './history-discovery.mjs';
import {
  classifyConversationHttpStatus,
  conversationRoute,
  isJsonMimeType,
  matchConversationResponse,
  selectFinalConversationResponseMeta,
} from './conversation-capture.mjs';

const root = new URL('./', import.meta.url);
const manifest = JSON.parse(fs.readFileSync(new URL('manifest.json', root), 'utf8'));
const worker = fs.readFileSync(new URL('service-worker.js', root), 'utf8');
const knownGoodDiscoveryRuntime = fs.readFileSync(
  new URL('known-good-discovery-v0.3.txt', root),
  'utf8',
);
const knownGoodHistoryClassifier = fs.readFileSync(
  new URL('known-good-history-discovery-v0.3.txt', root),
  'utf8',
);
const desktopBridge = fs.readFileSync(
  new URL('../../apps/desktop/src/account_bridge.rs', root),
  'utf8',
);

function workerMilliseconds(name) {
  const match = worker.match(new RegExp(`const ${name} = ([\\d_]+);`));
  if (!match) throw new Error(`missing worker duration constant ${name}`);
  return Number(match[1].replaceAll('_', ''));
}

function rustDurationMilliseconds(name) {
  const match = desktopBridge.match(
    new RegExp(`const ${name}: Duration = Duration::from_secs\\((\\d+)\\);`),
  );
  if (!match) throw new Error(`missing Rust duration constant ${name}`);
  return Number(match[1]) * 1_000;
}

function sameSet(actual, expected, label) {
  if (!Array.isArray(actual)) throw new Error(`${label} must be an array`);
  const left = [...actual].sort();
  const right = [...expected].sort();
  if (JSON.stringify(left) !== JSON.stringify(right)) {
    throw new Error(`${label} drifted: ${JSON.stringify(actual)}`);
  }
}

if (manifest.manifest_version !== 3) {
  throw new Error('Edge bridge must remain Manifest V3');
}
if (manifest.name !== 'Chatarium Edge Bridge') {
  throw new Error('unexpected extension name');
}
if (manifest.version !== '0.4.9') {
  throw new Error(`unexpected extension version ${manifest.version}`);
}
sameSet(manifest.permissions, ['debugger', 'scripting', 'storage'], 'permissions');
sameSet(
  manifest.host_permissions,
  ['https://chatgpt.com/*', 'http://127.0.0.1:43117/*'],
  'host_permissions',
);
if (manifest.background?.service_worker !== 'service-worker.js') {
  throw new Error('unexpected service worker entrypoint');
}
if (manifest.background?.type !== 'module') {
  throw new Error('service worker must remain an ES module');
}
for (const forbidden of ['GM_xmlhttpRequest', 'unsafeWindow', 'Tampermonkey']) {
  if (worker.includes(forbidden)) {
    throw new Error(`retired userscript dependency leaked into Edge bridge: ${forbidden}`);
  }
}
for (const required of [
  "world: 'MAIN'",
  'chrome.debugger.attach',
  "'Network.enable'",
  "'Network.getResponseBody'",
  'chrome.debugger.detach',
  "'Page.reload'",
  'chrome.storage.session',
  "const ACCOUNT_HEADER = 'ChatGPT-Account-ID'",
  "const BRIDGE_ORIGIN = 'http://127.0.0.1:43117'",
  "const BRIDGE_HEADER_VALUE = 'edge-mv3-v1'",
  "const DISCOVERY_PROFILE = 'cdp-history-discovery-v1'",
  'PAGE_FETCH_TIMEOUT_MS',
  "case 'discover_history_surfaces'",
  'chrome.tabs.reload(tab.id)',
  'chrome.tabs.create',
  "'Page.navigate'",
  'chrome.tabs.remove',
  'activeConversationCaptures',
  'captureConversationByNavigation',
  'classifyConversationHttpStatus',
  'rate_limited_responses',
  'rate_limit_reload_scheduled',
  'rate_limit_reload_count',
  'rate_limit_reload_failures',
  'CONVERSATION_RATE_LIMIT_RELOAD_DELAY_MS',
  'scheduleRateLimitReload',
  "'Page.reload'",
  'lastRateLimitMeta',
  'selectFinalConversationResponseMeta',
  'exact_response_count',
  "'exact_conversation_rate_limited'",
  'SERVICE_WORKER_KEEPALIVE_MS',
  'installServiceWorkerKeepalive',
  'chrome.runtime.getPlatformInfo',
  'settleWithin',
]) {
  if (!worker.includes(required)) {
    throw new Error(`required Edge bridge invariant missing: ${required}`);
  }
}
console.log('Chatarium Edge Bridge package invariants OK');


const discoveryModule = fs.readFileSync(new URL('history-discovery.mjs', root), 'utf8');
for (const required of [
  'export function isCandidateResponse',
  'export function classifyHistoryBody',
  'export function mergeDiscoveryCandidates',
  "surface_kind: classifySurfaceKind",
  'MAX_VISITED_NODES',
  'MAX_CONVERSATIONS',
]) {
  if (!discoveryModule.includes(required)) {
    throw new Error(`required CDP discovery invariant missing: ${required}`);
  }
}

function normalizeNewlines(text) {
  return text.replaceAll('\r\n', '\n');
}

function fixtureSection(text, startMarker, endMarker) {
  const normalized = normalizeNewlines(text);
  const start = normalized.indexOf(startMarker);
  const end = normalized.indexOf(endMarker, start + startMarker.length);
  if (start < 0 || end < 0) {
    throw new Error(`malformed known-good fixture section: ${startMarker}`);
  }
  let section = normalized.slice(start + startMarker.length, end);
  if (section.startsWith('\n')) section = section.slice(1);
  if (section.endsWith('\n')) section = section.slice(0, -1);
  return section;
}

const knownGoodDiscoveryListener = fixtureSection(
  knownGoodDiscoveryRuntime,
  '--- DISCOVERY LISTENER ---',
  '--- END DISCOVERY LISTENER ---',
);
const knownGoodDiscoveryCommand = fixtureSection(
  knownGoodDiscoveryRuntime,
  '--- DISCOVERY COMMAND ---',
  '--- END DISCOVERY COMMAND ---',
);
const knownGoodClassifierModule = fixtureSection(
  knownGoodHistoryClassifier,
  '--- HISTORY DISCOVERY MODULE ---',
  '--- END HISTORY DISCOVERY MODULE ---',
);

if (!normalizeNewlines(worker).includes(knownGoodDiscoveryListener)) {
  throw new Error(
    'history discovery debugger listener drifted from the live-proven 0.3 baseline',
  );
}
if (!normalizeNewlines(worker).includes(knownGoodDiscoveryCommand)) {
  throw new Error(
    'discoverHistorySurfaces drifted from the live-proven 0.3 baseline',
  );
}
if (normalizeNewlines(discoveryModule) !== knownGoodClassifierModule) {
  throw new Error(
    'history-discovery.mjs drifted from the live-proven 0.3 baseline',
  );
}

console.log('Chatarium live-proven 0.3 discovery baseline is frozen');


const ordinaryResponse = {
  status: 200,
  mimeType: 'application/json',
  url: 'https://chatgpt.com/backend-api/conversations?limit=20&offset=0',
};
if (!isCandidateResponse(ordinaryResponse)) {
  throw new Error('ordinary conversation response should be a discovery candidate');
}
if (isCandidateResponse({ ...ordinaryResponse, status: 429 })) {
  throw new Error('rate-limited response must not be promoted to a discovery candidate');
}
if (isCandidateResponse({
  status: 200,
  mimeType: 'application/json',
  url: 'https://chatgpt.com/backend-api/settings/user',
})) {
  throw new Error('known-good passive discovery endpoint filter drifted');
}

const sidebar = classifyHistoryBody(
  'https://chatgpt.com/backend-api/gizmos/snorlax/sidebar?conversations_per_gizmo=5&limit=20&owned_only=false',
  {
    items: [
      {
        gizmo: { gizmo: { id: 'project-1' } },
        conversations: {
          items: [
            {
              id: 'conversation-1',
              title: 'One',
              create_time: '2026-10-03T00:00:00Z',
              update_time: '2026-10-03T00:01:00Z',
            },
            {
              id: 'conversation-2',
              title: null,
              create_time: '2026-10-02T00:00:00Z',
              update_time: '2026-10-02T00:01:00Z',
            },
          ],
          cursor: 'nested-next',
        },
      },
    ],
    cursor: 'top-next',
  },
);
if (!sidebar || sidebar.surface_kind !== 'snorlax_sidebar') {
  throw new Error('snorlax sidebar surface classification failed');
}
if (sidebar.conversation_count !== 2 || sidebar.items.length !== 2) {
  throw new Error('snorlax sidebar conversation extraction failed');
}
if (sidebar.cursor_count !== 2 || sidebar.top_level_cursor !== 'string') {
  throw new Error('snorlax sidebar cursor classification failed');
}
if (sidebar.query_keys.join(',') !== 'conversations_per_gizmo,limit,owned_only') {
  throw new Error('query values leaked into discovery metadata or query-key order drifted');
}

const merged = mergeDiscoveryCandidates([
  sidebar,
  {
    ...sidebar,
    conversation_count: 1,
    items: [
      {
        id: 'conversation-3',
        title: 'Three',
        create_time: null,
        update_time: null,
      },
    ],
  },
]);
if (merged.length !== 1 || merged[0].observations !== 2) {
  throw new Error('repeated observations were not merged by surface shape');
}
if (merged[0].items.length !== 3) {
  throw new Error('conversation identities were not merged across repeated observations');
}

console.log('Chatarium CDP history discovery classifier OK');


const route = conversationRoute('conversation / one');
if (route !== 'https://chatgpt.com/c/conversation%20%2F%20one') {
  throw new Error(`conversation route encoding drifted: ${route}`);
}
if (conversationRoute('') !== null || conversationRoute('x'.repeat(257)) !== null) {
  throw new Error('invalid conversation ids should not produce navigation routes');
}

const exactConversation = matchConversationResponse(
  {
    status: 200,
    mimeType: 'application/json',
    url: 'https://chatgpt.com/backend-api/conversations/remote-1?num_turns=10&include_has_versions=true',
  },
  'remote-1',
);
if (!exactConversation) {
  throw new Error('exact first-party conversation response was not matched');
}
if (exactConversation.http_status !== 200) {
  throw new Error('exact conversation status was not preserved');
}
if (exactConversation.query_keys.join(',') !== 'num_turns,include_has_versions') {
  throw new Error('exact conversation query-key evidence drifted');
}
if (matchConversationResponse(
  {
    status: 200,
    mimeType: 'application/json',
    url: 'https://chatgpt.com/backend-api/conversations/remote-2',
  },
  'remote-1',
) !== null) {
  throw new Error('wrong remote conversation id matched exact capture');
}
if (matchConversationResponse(
  {
    status: 200,
    mimeType: 'application/json',
    url: 'https://example.com/backend-api/conversations/remote-1',
  },
  'remote-1',
) !== null) {
  throw new Error('off-origin response matched exact capture');
}
if (!isJsonMimeType('application/json; charset=utf-8') || !isJsonMimeType('application/problem+json')) {
  throw new Error('JSON media-type classifier rejected supported JSON');
}
if (isJsonMimeType('text/html')) {
  throw new Error('non-JSON media type passed conversation capture gate');
}

const successfulMeta = { http_status: 200, mime_type: 'application/json' };
const lateRateLimitMeta = { http_status: 429, mime_type: 'application/json' };
if (
  selectFinalConversationResponseMeta(successfulMeta, lateRateLimitMeta)
    !== successfulMeta
) {
  throw new Error('late HTTP 429 must not overwrite successful exact response proof');
}
if (
  selectFinalConversationResponseMeta(null, lateRateLimitMeta)
    !== lateRateLimitMeta
) {
  throw new Error('rate-limit proof must survive when no successful exact response exists');
}

if (classifyConversationHttpStatus(200) !== 'success') {
  throw new Error('HTTP 200 conversation response must be capturable');
}
if (classifyConversationHttpStatus(429) !== 'transient_rate_limit') {
  throw new Error('HTTP 429 conversation response must stay observable, not terminal');
}
for (const status of [400, 401, 403, 404, 500, 503]) {
  if (classifyConversationHttpStatus(status) !== 'terminal_http_error') {
    throw new Error(`HTTP ${status} must remain terminal for exact conversation capture`);
  }
}

console.log('Chatarium CDP conversation capture matcher OK');


const debuggerEventListenerCount =
  worker.match(/chrome\.debugger\.onEvent\.addListener/g)?.length ?? 0;
if (debuggerEventListenerCount < 2) {
  throw new Error(
    'history discovery and exact mirroring must use isolated debugger event listeners',
  );
}
if (!worker.includes('const session = activeDiscoveries.get(tabId);')) {
  throw new Error('known-good discovery listener is not isolated on activeDiscoveries');
}
if (!worker.includes('const capture = activeConversationCaptures.get(tabId);')) {
  throw new Error('exact mirror listener is not isolated on activeConversationCaptures');
}


if (!worker.includes('const SERVICE_WORKER_KEEPALIVE_MS = 20_000')) {
  throw new Error('MV3 bridge keepalive must remain below the 30-second idle window');
}
if (!worker.includes('void chrome.runtime.getPlatformInfo().catch(() => {})')) {
  throw new Error('MV3 bridge keepalive must use a local extension API heartbeat');
}
const keepaliveInstallIndex = worker.lastIndexOf('installServiceWorkerKeepalive();');
const bridgeLoopInstallIndex = worker.lastIndexOf('ensureBridgeLoop();');
if (
  keepaliveInstallIndex < 0
  || bridgeLoopInstallIndex < 0
  || keepaliveInstallIndex > bridgeLoopInstallIndex
) {
  throw new Error('MV3 bridge keepalive must start before relying on the loopback bridge');
}

if (!worker.includes('const CONVERSATION_RATE_LIMIT_RELOAD_DELAY_MS = 12_000')) {
  throw new Error('exact mirror 429 recovery delay must remain bounded at 12 seconds');
}
if (!worker.includes('const CONVERSATION_CAPTURE_WINDOW_MS = 32_000')) {
  throw new Error('exact mirror capture window must leave room for one delayed first-party reload');
}
const scheduleBody = worker.slice(
  worker.indexOf('async function scheduleRateLimitReload'),
  worker.indexOf('async function captureConversationByNavigation'),
);
const reloadCalls = scheduleBody.match(/'Page\.reload'/g)?.length ?? 0;
if (reloadCalls !== 1) {
  throw new Error(
    `rate-limit recovery must issue exactly one browser-level reload, found ${reloadCalls}`,
  );
}
if (!scheduleBody.includes('if (capture.rate_limit_reload_scheduled) return;')) {
  throw new Error('rate-limit recovery must be single-flight');
}

if (worker.includes('SIDEBAR_BOOTSTRAP_RESOURCE')) {
  throw new Error('synthetic sidebar history replay must remain retired');
}
if (worker.includes('stimulateHistoryUi')) {
  throw new Error('post-0.3 history UI stimulus must remain outside the frozen discovery path');
}
if (worker.includes("'Network.setCacheDisabled'")) {
  throw new Error('post-0.3 discovery cache-bypass mutation must remain retired');
}

const discoveryWindowMs = workerMilliseconds('DISCOVERY_WINDOW_MS');
const discoveryBodyGraceMs = workerMilliseconds('DISCOVERY_BODY_GRACE_MS');
const discoveryWaitMs = rustDurationMilliseconds('DISCOVERY_RESULT_WAIT');
const discoveryWorstCaseMs = discoveryWindowMs + discoveryBodyGraceMs;
if (discoveryWorstCaseMs + 5_000 >= discoveryWaitMs) {
  throw new Error(
    `frozen 0.3 discovery can outlive desktop result wait: worst=${discoveryWorstCaseMs}ms wait=${discoveryWaitMs}ms`,
  );
}

const pageFetchTimeoutMs = workerMilliseconds('PAGE_FETCH_TIMEOUT_MS');
const authWaitMs = rustDurationMilliseconds('AUTH_RESULT_WAIT');
if (authWaitMs < pageFetchTimeoutMs + 2_000) {
  throw new Error(
    `authentication wait must exceed page fetch bound with margin: page=${pageFetchTimeoutMs}ms wait=${authWaitMs}ms`,
  );
}
