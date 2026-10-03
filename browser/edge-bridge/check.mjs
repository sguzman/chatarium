import fs from 'node:fs';
import {
  classifyHistoryBody,
  isCandidateResponse,
  mergeDiscoveryCandidates,
} from './history-discovery.mjs';
import {
  conversationRoute,
  isJsonMimeType,
  matchConversationResponse,
} from './conversation-capture.mjs';

const root = new URL('./', import.meta.url);
const manifest = JSON.parse(fs.readFileSync(new URL('manifest.json', root), 'utf8'));
const worker = fs.readFileSync(new URL('service-worker.js', root), 'utf8');

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
if (manifest.version !== '0.4.3') {
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
  'chrome.tabs.reload',
  'chrome.storage.session',
  "const ACCOUNT_HEADER = 'ChatGPT-Account-ID'",
  "const BRIDGE_ORIGIN = 'http://127.0.0.1:43117'",
  "const BRIDGE_HEADER_VALUE = 'edge-mv3-v1'",
  "const DISCOVERY_PROFILE = 'cdp-history-discovery-v1'",
  "const SIDEBAR_BOOTSTRAP_RESOURCE =",
  'collectApplicationContextHeaders',
  'if (passiveConversationCount === 0)',
  'PAGE_FETCH_TIMEOUT_MS',
  'sidebar_bootstrap_attempted',
  'sidebar_bootstrap_http_status',
  'sidebar_bootstrap_items',
  "case 'discover_history_surfaces'",
  'chrome.tabs.create',
  "'Page.navigate'",
  'chrome.tabs.remove',
  'activeConversationCaptures',
  'captureConversationByNavigation',
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


const sidebarBootstrapPath =
  '/backend-api/gizmos/snorlax/sidebar?conversations_per_gizmo=5&limit=20&owned_only=false';
const sidebarBootstrap = classifyHistoryBody(
  `https://chatgpt.com${sidebarBootstrapPath}`,
  {
    items: [
      {
        gizmo: { gizmo: { id: 'project-bootstrap' } },
        conversations: {
          items: [
            {
              id: 'bootstrap-conversation-1',
              title: 'Bootstrap one',
              create_time: '2026-10-03T00:00:00Z',
              update_time: '2026-10-03T00:01:00Z',
            },
            {
              id: 'bootstrap-conversation-2',
              title: 'Bootstrap two',
              create_time: '2026-10-03T00:02:00Z',
              update_time: '2026-10-03T00:03:00Z',
            },
          ],
          cursor: null,
        },
      },
    ],
    cursor: 'next-project-page',
  },
);
if (!sidebarBootstrap || sidebarBootstrap.surface_kind !== 'snorlax_sidebar') {
  throw new Error('evidence-backed sidebar bootstrap surface was not classified');
}
if (sidebarBootstrap.conversation_count !== 2) {
  throw new Error('evidence-backed sidebar bootstrap conversations were not extracted');
}
