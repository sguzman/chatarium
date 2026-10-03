'use strict';

const BRIDGE_ORIGIN = 'http://127.0.0.1:43117';
const BRIDGE_HEADER = 'X-Chatarium-Bridge';
const BRIDGE_HEADER_VALUE = 'edge-mv3-v1';
const PROTOCOL_VERSION = 1;
const EXTENSION_VERSION = chrome.runtime.getManifest().version;
const ACCOUNT_HEADER = 'ChatGPT-Account-ID';
const ACCOUNT_KEY_PREFIX = 'chatarium-account-context:';
const LIST_CONTEXT_KEY_PREFIX = 'chatarium-list-request-context:';
const LIST_RESOURCE =
  '/backend-api/conversations?exclude_conversation_origin=tpp&expand=false&hide_snorlax=false&is_archived=false&is_starred=false&limit=20&order=updated&offset=0';
const LIST_PROFILE = '2026-10-03.003';
const FETCH_PROFILE = '2026-10-03.001';
const AUTH_PROFILE = 'chatgpt-me-v1';
const MAX_RESPONSE_BYTES = 4 * 1024 * 1024;
const NEXT_TIMEOUT_MS = 29_000;
const RESULT_TIMEOUT_MS = 10_000;
const LOOPBACK_RETRY_MS = 1_000;
let loopRunning = false;
let lastLoopbackError = '';
const replayingTabs = new Set();

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function accountKey(tabId) {
  return `${ACCOUNT_KEY_PREFIX}${tabId}`;
}

function listContextKey(tabId) {
  return `${LIST_CONTEXT_KEY_PREFIX}${tabId}`;
}

function validAccountId(value) {
  return typeof value === 'string'
    && value.length > 0
    && value.length <= 128
    && /^[A-Za-z0-9_-]+$/.test(value);
}

async function rememberAccountContext(tabId, value) {
  if (!Number.isInteger(tabId) || tabId < 0 || !validAccountId(value)) return;
  await chrome.storage.session.set({
    [accountKey(tabId)]: value,
  });
}

async function forgetAccountContext(tabId) {
  if (!Number.isInteger(tabId) || tabId < 0) return;
  await chrome.storage.session.remove([accountKey(tabId), listContextKey(tabId)]);
}

function safeListResource(rawUrl) {
  let url;
  try {
    url = new URL(rawUrl);
  } catch {
    return null;
  }
  if (url.origin !== 'https://chatgpt.com' || url.pathname !== '/backend-api/conversations') {
    return null;
  }
  const expected = new URL(`https://chatgpt.com${LIST_RESOURCE}`);
  if (url.search !== expected.search) {
    return null;
  }
  return `${url.pathname}${url.search}`;
}

function isReplayHeaderName(name) {
  const lower = name.toLowerCase();
  return lower === 'chatgpt-account-id'
    || lower === 'oai-did'
    || lower === 'oai-language'
    || lower === 'originator'
    || lower.startsWith('x-oai-')
    || lower.startsWith('x-openai-');
}

function collectReplayHeaders(requestHeaders) {
  const headers = {};
  for (const header of requestHeaders ?? []) {
    if (typeof header.name !== 'string' || typeof header.value !== 'string') continue;
    if (!isReplayHeaderName(header.name)) continue;
    if (header.value.length === 0 || header.value.length > 2048) continue;
    headers[header.name] = header.value;
  }
  return headers;
}

async function rememberListRequestContext(details) {
  if (!Number.isInteger(details.tabId) || details.tabId < 0 || replayingTabs.has(details.tabId)) {
    return;
  }
  if (details.method !== 'GET') return;
  const resource = safeListResource(details.url);
  if (resource === null) return;

  const headers = collectReplayHeaders(details.requestHeaders);
  const accountEntry = Object.entries(headers).find(
    ([name]) => name.toLowerCase() === ACCOUNT_HEADER.toLowerCase(),
  );
  const accountId = accountEntry?.[1];
  if (!validAccountId(accountId)) return;

  await rememberAccountContext(details.tabId, accountId);
  await chrome.storage.session.set({
    [listContextKey(details.tabId)]: {
      resource,
      headers,
      observed_status: null,
    },
  });
}

async function rememberListCompletion(details) {
  if (!Number.isInteger(details.tabId) || details.tabId < 0 || replayingTabs.has(details.tabId)) {
    return;
  }
  const resource = safeListResource(details.url);
  if (resource === null) return;
  const key = listContextKey(details.tabId);
  const stored = await chrome.storage.session.get(key);
  const context = stored[key];
  if (!context || context.resource !== resource || typeof context.headers !== 'object') return;
  await chrome.storage.session.set({
    [key]: {
      ...context,
      observed_status: Number.isInteger(details.statusCode) ? details.statusCode : null,
    },
  });
}

chrome.webRequest.onBeforeSendHeaders.addListener(
  (details) => {
    if (!Number.isInteger(details.tabId) || details.tabId < 0) return;
    const header = details.requestHeaders?.find(
      ({ name }) => name.toLowerCase() === ACCOUNT_HEADER.toLowerCase(),
    );
    if (header && validAccountId(header.value)) {
      void rememberAccountContext(details.tabId, header.value);
    }
    void rememberListRequestContext(details);
  },
  { urls: ['https://chatgpt.com/backend-api/*'] },
  ['requestHeaders'],
);

chrome.webRequest.onCompleted.addListener(
  (details) => {
    void rememberListCompletion(details);
  },
  { urls: ['https://chatgpt.com/backend-api/conversations?*'] },
);

chrome.tabs.onRemoved.addListener((tabId) => {
  void forgetAccountContext(tabId);
});

chrome.webRequest.onBeforeRequest.addListener(
  (details) => {
    if (details.type === 'main_frame' && Number.isInteger(details.tabId) && details.tabId >= 0) {
      void forgetAccountContext(details.tabId);
    }
  },
  { urls: ['https://chatgpt.com/*'], types: ['main_frame'] },
);

async function accountContextForTab(tabId) {
  const key = accountKey(tabId);
  const stored = await chrome.storage.session.get(key);
  const value = stored[key];
  return validAccountId(value) ? value : null;
}

async function listRequestContextForTab(tabId) {
  const key = listContextKey(tabId);
  const stored = await chrome.storage.session.get(key);
  const context = stored[key];
  if (!context || context.resource !== LIST_RESOURCE || typeof context.headers !== 'object') {
    return null;
  }
  const accountEntry = Object.entries(context.headers).find(
    ([name]) => name.toLowerCase() === ACCOUNT_HEADER.toLowerCase(),
  );
  if (!validAccountId(accountEntry?.[1])) return null;
  return {
    resource: context.resource,
    headers: context.headers,
    observedStatus: Number.isInteger(context.observed_status)
      ? context.observed_status
      : null,
  };
}

async function findChatGptTab() {
  const tabs = await chrome.tabs.query({ url: 'https://chatgpt.com/*' });
  const candidates = tabs.filter((tab) => Number.isInteger(tab.id));
  if (candidates.length === 0) {
    return { tab: null, accountId: null };
  }

  const withContext = [];
  for (const tab of candidates) {
    const accountId = await accountContextForTab(tab.id);
    if (accountId !== null) {
      withContext.push({ tab, accountId });
    }
  }

  if (withContext.length > 0) {
    return withContext.find(({ tab }) => tab.active) ?? withContext[0];
  }

  return {
    tab: candidates.find((tab) => tab.active) ?? candidates[0],
    accountId: null,
  };
}

function baseResult(command, requestProfile) {
  return {
    version: PROTOCOL_VERSION,
    id: command.id,
    kind: command.kind,
    ok: false,
    bridge_transport: 'extension',
    extension_version: EXTENSION_VERSION,
    chatgpt_tab_found: false,
    main_world_execution: false,
    account_context: false,
    request_context_observed: false,
    first_party_http_status: null,
    context_header_count: 0,
    request_profile: requestProfile,
  };
}

async function pageGet(resource, requestHeaders, maxResponseBytes) {
  try {
    const response = await fetch(resource, {
      method: 'GET',
      headers: requestHeaders,
      credentials: 'include',
      cache: 'no-store',
      redirect: 'error',
    });
    const contentType = response.headers.get('content-type') ?? '';
    const declaredLength = Number(response.headers.get('content-length'));
    if (Number.isFinite(declaredLength) && declaredLength > maxResponseBytes) {
      return {
        ok: false,
        http_status: response.status,
        content_type: contentType,
        error: 'remote_response_too_large',
      };
    }

    const text = await response.text();
    if (new TextEncoder().encode(text).byteLength > maxResponseBytes) {
      return {
        ok: false,
        http_status: response.status,
        content_type: contentType,
        error: 'remote_response_too_large',
      };
    }

    const mediaType = contentType.toLowerCase().split(';', 1)[0].trim();
    let body = null;
    if (text.length > 0 && mediaType.endsWith('json')) {
      try {
        body = JSON.parse(text);
      } catch {
        return {
          ok: false,
          http_status: response.status,
          content_type: contentType,
          error: 'remote_invalid_json',
        };
      }
    }

    return {
      ok: response.status === 200,
      http_status: response.status,
      content_type: contentType,
      body,
      error: response.status === 200 ? null : 'remote_http_status',
    };
  } catch (error) {
    return {
      ok: false,
      error: error instanceof Error ? error.message : 'remote_fetch_failed',
    };
  }
}

async function executePageGet(tabId, resource, requestHeaders) {
  replayingTabs.add(tabId);
  try {
    const injection = await chrome.scripting.executeScript({
      target: { tabId },
      world: 'MAIN',
      func: pageGet,
      args: [resource, requestHeaders, MAX_RESPONSE_BYTES],
    });
    if (!Array.isArray(injection) || injection.length !== 1 || !injection[0]) {
      throw new Error('main_world_no_result');
    }
    return injection[0].result;
  } finally {
    replayingTabs.delete(tabId);
  }
}

async function executeRead(command, resource, requestProfile, requireObservedContext = false) {
  const result = baseResult(command, requestProfile);
  const { tab, accountId } = await findChatGptTab();
  if (!tab) {
    result.error = 'chatgpt_tab_not_found';
    return result;
  }
  result.chatgpt_tab_found = true;

  if (accountId === null) {
    result.error = 'account_context_unavailable';
    return result;
  }
  result.account_context = true;

  let requestHeaders = { [ACCOUNT_HEADER]: accountId };
  if (requireObservedContext) {
    const context = await listRequestContextForTab(tab.id);
    if (context === null) {
      result.error = 'first_party_request_context_unavailable';
      return result;
    }
    const accountEntry = Object.entries(context.headers).find(
      ([name]) => name.toLowerCase() === ACCOUNT_HEADER.toLowerCase(),
    );
    if (accountEntry?.[1] !== accountId) {
      result.error = 'first_party_request_context_account_mismatch';
      return result;
    }
    requestHeaders = context.headers;
    result.request_context_observed = true;
    result.first_party_http_status = context.observedStatus;
    result.context_header_count = Object.keys(context.headers).length;
  }

  let pageResult;
  try {
    pageResult = await executePageGet(tab.id, resource, requestHeaders);
    result.main_world_execution = true;
  } catch (error) {
    result.error = error instanceof Error ? error.message : 'main_world_execution_failed';
    return result;
  }

  if (!pageResult || typeof pageResult !== 'object') {
    result.error = 'main_world_invalid_result';
    return result;
  }

  result.http_status = pageResult.http_status;
  result.content_type = pageResult.content_type;
  if (pageResult.ok !== true) {
    result.error = typeof pageResult.error === 'string'
      ? pageResult.error
      : 'remote_fetch_failed';
    return result;
  }

  result.ok = true;
  result.body = pageResult.body;
  return result;
}

async function probeAuthentication(command) {
  const result = await executeRead(command, '/backend-api/me', AUTH_PROFILE);
  const status = Number.isInteger(result.http_status) ? result.http_status : null;
  result.authentication = status === 200
    ? 'authenticated'
    : (status === 401 || status === 403)
      ? 'unauthenticated'
      : 'unknown';

  if (status === 401 || status === 403) {
    result.ok = true;
  }
  return result;
}

async function listConversations(command) {
  if (command.resource !== LIST_RESOURCE || command.request_profile !== LIST_PROFILE) {
    return {
      ...baseResult(command, LIST_PROFILE),
      error: 'resource_profile_mismatch',
    };
  }
  return executeRead(command, LIST_RESOURCE, LIST_PROFILE, true);
}

function percentEncodePathSegment(value) {
  const bytes = new TextEncoder().encode(value);
  let encoded = '';
  for (const byte of bytes) {
    const ascii =
      (byte >= 0x41 && byte <= 0x5a)
      || (byte >= 0x61 && byte <= 0x7a)
      || (byte >= 0x30 && byte <= 0x39)
      || byte === 0x2d
      || byte === 0x2e
      || byte === 0x5f
      || byte === 0x7e;
    encoded += ascii ? String.fromCharCode(byte) : `%${byte.toString(16).toUpperCase().padStart(2, '0')}`;
  }
  return encoded;
}

async function fetchConversation(command) {
  const remoteId = typeof command.remote_conversation_id === 'string'
    ? command.remote_conversation_id
    : '';
  if (!remoteId) {
    return {
      ...baseResult(command, FETCH_PROFILE),
      error: 'missing_remote_conversation_id',
    };
  }

  const expectedResource =
    `/backend-api/conversations/${percentEncodePathSegment(remoteId)}?num_turns=10&include_has_versions=true`;
  if (command.resource !== expectedResource || command.request_profile !== FETCH_PROFILE) {
    return {
      ...baseResult(command, FETCH_PROFILE),
      error: 'resource_profile_mismatch',
    };
  }
  return executeRead(command, expectedResource, FETCH_PROFILE, true);
}

async function execute(command) {
  if (!command || command.version !== PROTOCOL_VERSION || typeof command.id !== 'string') {
    return null;
  }

  switch (command.kind) {
    case 'probe_auth':
      return probeAuthentication(command);
    case 'list_conversations':
      return listConversations(command);
    case 'fetch_conversation':
      return fetchConversation(command);
    default:
      return {
        ...baseResult(command, 'unsupported'),
        error: 'unsupported_command',
      };
  }
}

async function loopbackRequest(method, path, body = null, timeoutMs = NEXT_TIMEOUT_MS) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort('Chatarium loopback timeout'), timeoutMs);
  try {
    const response = await fetch(`${BRIDGE_ORIGIN}${path}`, {
      method,
      headers: {
        [BRIDGE_HEADER]: BRIDGE_HEADER_VALUE,
        ...(body === null ? {} : { 'Content-Type': 'application/json' }),
      },
      body: body === null ? undefined : JSON.stringify(body),
      credentials: 'omit',
      cache: 'no-store',
      redirect: 'error',
      signal: controller.signal,
    });
    return response;
  } finally {
    clearTimeout(timer);
  }
}

async function bridgeLoop() {
  while (true) {
    try {
      const response = await loopbackRequest('GET', '/v1/next');
      if (response.status === 204) {
        lastLoopbackError = '';
        continue;
      }
      if (response.status !== 200) {
        throw new Error(`loopback_next_http_${response.status}`);
      }

      let command;
      try {
        command = await response.json();
      } catch {
        throw new Error('loopback_invalid_command_json');
      }

      const result = await execute(command);
      if (!result) {
        throw new Error('loopback_invalid_command');
      }

      const posted = await loopbackRequest('POST', '/v1/result', result, RESULT_TIMEOUT_MS);
      if (posted.status !== 204) {
        throw new Error(`loopback_result_http_${posted.status}`);
      }
      lastLoopbackError = '';
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      if (detail !== lastLoopbackError) {
        console.warn('[Chatarium Edge Bridge]', detail);
        lastLoopbackError = detail;
      }
      await sleep(LOOPBACK_RETRY_MS);
    }
  }
}

function ensureBridgeLoop() {
  if (loopRunning) return;
  loopRunning = true;
  void bridgeLoop().finally(() => {
    loopRunning = false;
  });
}

chrome.runtime.onInstalled.addListener(ensureBridgeLoop);
chrome.runtime.onStartup.addListener(ensureBridgeLoop);
ensureBridgeLoop();
