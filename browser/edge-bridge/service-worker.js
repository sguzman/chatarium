'use strict';

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

const BRIDGE_ORIGIN = 'http://127.0.0.1:43117';
const BRIDGE_HEADER = 'X-Chatarium-Bridge';
const BRIDGE_HEADER_VALUE = 'edge-mv3-v1';
const PROTOCOL_VERSION = 1;
const EXTENSION_VERSION = chrome.runtime.getManifest().version;
const ACCOUNT_HEADER = 'ChatGPT-Account-ID';
const ACCOUNT_KEY_PREFIX = 'chatarium-account-context:';
const FETCH_PROFILE = '2026-10-03.001';
const AUTH_PROFILE = 'chatgpt-me-v1';
const DISCOVERY_PROFILE = 'cdp-history-discovery-v1';
const FRESH_DISCOVERY_PROFILE = 'cdp-history-fresh-tab-v1';
const FRESH_DISCOVERY_ROUTE = 'https://chatgpt.com/';
const DEBUGGER_PROTOCOL_VERSION = '1.3';
const DISCOVERY_WINDOW_MS = 8_000;
const DISCOVERY_BODY_GRACE_MS = 500;
const CONVERSATION_CAPTURE_WINDOW_MS = 32_000;
const CONVERSATION_CAPTURE_BODY_GRACE_MS = 500;
const CONVERSATION_RATE_LIMIT_RELOAD_DELAY_MS = 12_000;
const MAX_RESPONSE_BYTES = 4 * 1024 * 1024;
const NEXT_TIMEOUT_MS = 29_000;
const RESULT_TIMEOUT_MS = 10_000;
const LOOPBACK_RETRY_MS = 1_000;
const SERVICE_WORKER_KEEPALIVE_MS = 20_000;
const PAGE_FETCH_TIMEOUT_MS = 8_000;

let loopRunning = false;
let lastLoopbackError = '';
const activeDiscoveries = new Map();
const activeConversationCaptures = new Map();

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function settleWithin(promises, timeoutMs) {
  const tasks = [...promises];
  if (tasks.length === 0) return;
  await Promise.race([
    Promise.allSettled(tasks),
    sleep(timeoutMs),
  ]);
}

function accountKey(tabId) {
  return `${ACCOUNT_KEY_PREFIX}${tabId}`;
}

function validAccountId(value) {
  return typeof value === 'string'
    && value.length > 0
    && value.length <= 128
    && /^[A-Za-z0-9_-]+$/.test(value);
}

async function rememberAccountContext(tabId, value) {
  if (!Number.isInteger(tabId) || tabId < 0 || !validAccountId(value)) return;
  await chrome.storage.session.set({ [accountKey(tabId)]: value });
}

async function forgetAccountContext(tabId) {
  if (!Number.isInteger(tabId) || tabId < 0) return;
  await chrome.storage.session.remove(accountKey(tabId));
}

async function accountContextForTab(tabId) {
  const key = accountKey(tabId);
  const stored = await chrome.storage.session.get(key);
  const value = stored[key];
  return validAccountId(value) ? value : null;
}

chrome.tabs.onRemoved.addListener((tabId) => {
  void forgetAccountContext(tabId);
});

async function findChatGptTab() {
  const tabs = await chrome.tabs.query({ url: 'https://chatgpt.com/*' });
  const candidates = tabs.filter((tab) => Number.isInteger(tab.id));
  if (candidates.length === 0) return null;
  return candidates.find((tab) => tab.active) ?? candidates[0];
}

function baseResult(command, requestProfile) {
  return {
    version: PROTOCOL_VERSION,
    id: command.id,
    kind: command.kind,
    ok: false,
    bridge_transport: 'extension-cdp',
    extension_version: EXTENSION_VERSION,
    chatgpt_tab_found: false,
    main_world_execution: false,
    account_context: false,
    debugger_attached: false,
    network_enabled: false,
    reload_started: false,
    request_profile: requestProfile,
  };
}

function headerValue(headers, name) {
  if (!headers || typeof headers !== 'object') return null;
  const wanted = name.toLowerCase();
  for (const [key, value] of Object.entries(headers)) {
    if (key.toLowerCase() !== wanted) continue;
    return typeof value === 'string' ? value : null;
  }
  return null;
}

function safeBackendUrl(rawUrl) {
  let url;
  try {
    url = new URL(rawUrl);
  } catch {
    return null;
  }
  if (url.origin !== 'https://chatgpt.com') return null;
  if (!url.pathname.startsWith('/backend-api/')) return null;
  return url;
}

async function pageGet(resource, requestHeaders, maxResponseBytes, timeoutMs) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort('page fetch timeout'), timeoutMs);
  try {
    const response = await fetch(resource, {
      method: 'GET',
      headers: requestHeaders,
      credentials: 'include',
      cache: 'no-store',
      redirect: 'error',
      signal: controller.signal,
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
  } finally {
    clearTimeout(timer);
  }
}

async function executePageGet(tabId, resource, requestHeaders) {
  const injection = await chrome.scripting.executeScript({
    target: { tabId },
    world: 'MAIN',
    func: pageGet,
    args: [resource, requestHeaders, MAX_RESPONSE_BYTES, PAGE_FETCH_TIMEOUT_MS],
  });
  if (!Array.isArray(injection) || injection.length !== 1 || !injection[0]) {
    throw new Error('main_world_no_result');
  }
  return injection[0].result;
}

async function discoverHistorySurfacesFreshTab(command) {
  const result = baseResult(command, FRESH_DISCOVERY_PROFILE);
  result.capture_tab_created = false;
  result.navigation_started = false;

  const sourceTab = await findChatGptTab();
  if (!sourceTab) {
    result.error = 'chatgpt_tab_not_found';
    return result;
  }
  result.chatgpt_tab_found = true;
  const sourceAccountId = await accountContextForTab(sourceTab.id);

  let captureTab = null;
  let attached = false;
  let session = null;

  try {
    captureTab = await chrome.tabs.create({
      active: false,
      url: 'about:blank',
      ...(Number.isInteger(sourceTab.windowId) ? { windowId: sourceTab.windowId } : {}),
    });
    if (!captureTab || !Number.isInteger(captureTab.id)) {
      result.error = 'fresh_history_tab_creation_failed';
      return result;
    }
    result.capture_tab_created = true;

    const debuggee = { tabId: captureTab.id };
    session = {
      debuggee,
      tabId: captureTab.id,
      closed: false,
      accountId: null,
      responses_seen: 0,
      backend_200_seen: 0,
      json_candidates_seen: 0,
      body_read_failures: 0,
      body_too_large: 0,
      invalid_json: 0,
      detached_reason: null,
      pendingCandidates: new Map(),
      bodyTasks: new Set(),
      candidates: [],
    };

    if (activeDiscoveries.has(captureTab.id)) {
      result.error = 'fresh_history_discovery_already_active';
      return result;
    }
    activeDiscoveries.set(captureTab.id, session);

    await chrome.debugger.attach(debuggee, DEBUGGER_PROTOCOL_VERSION);
    attached = true;
    result.debugger_attached = true;

    await chrome.debugger.sendCommand(debuggee, 'Network.enable', {
      maxTotalBufferSize: 16 * 1024 * 1024,
      maxResourceBufferSize: MAX_RESPONSE_BYTES,
      maxPostDataSize: 0,
    });
    result.network_enabled = true;

    await chrome.debugger.sendCommand(debuggee, 'Page.enable');
    await chrome.debugger.sendCommand(debuggee, 'Page.navigate', {
      url: FRESH_DISCOVERY_ROUTE,
    });
    result.navigation_started = true;

    await sleep(DISCOVERY_WINDOW_MS);
    await sleep(DISCOVERY_BODY_GRACE_MS);
    if (session.bodyTasks.size > 0) {
      await Promise.allSettled([...session.bodyTasks]);
    }
  } catch (error) {
    result.error = error instanceof Error ? error.message : 'cdp_fresh_history_discovery_failed';
  } finally {
    if (session !== null) {
      session.closed = true;
      activeDiscoveries.delete(session.tabId);
    }

    result.account_context = (session?.accountId ?? sourceAccountId) !== null;
    result.responses_seen = session?.responses_seen ?? 0;
    result.backend_http_200_seen = session?.backend_200_seen ?? 0;
    result.json_candidates_seen = session?.json_candidates_seen ?? 0;
    result.body_read_failures = session?.body_read_failures ?? 0;
    result.body_too_large = session?.body_too_large ?? 0;
    result.invalid_json = session?.invalid_json ?? 0;
    result.detached_reason = session?.detached_reason ?? null;

    const candidates = mergeDiscoveryCandidates(session?.candidates ?? []);
    result.candidate_count = candidates.length;
    result.candidates = candidates;

    if (attached && captureTab && Number.isInteger(captureTab.id)) {
      try {
        await chrome.debugger.detach({ tabId: captureTab.id });
      } catch {
        // Navigation, browser shutdown, or tab teardown may already have detached the debugger.
      }
    }
    if (captureTab && Number.isInteger(captureTab.id)) {
      try {
        await chrome.tabs.remove(captureTab.id);
      } catch {
        // The temporary discovery tab may already be gone.
      }
    }
  }

  if (result.error) return result;
  if (
    !result.capture_tab_created
    || !result.debugger_attached
    || !result.network_enabled
    || !result.navigation_started
  ) {
    result.error = 'cdp_fresh_history_proof_incomplete';
    return result;
  }
  if (result.responses_seen === 0) {
    result.error = 'cdp_fresh_history_no_network_responses';
    return result;
  }

  result.ok = true;
  result.discovery = result.candidate_count > 0
    ? 'candidates_observed'
    : 'no_candidate_surface_observed';
  return result;
}

async function probeAuthentication(command) {
  const result = baseResult(command, AUTH_PROFILE);
  const tab = await findChatGptTab();
  if (!tab) {
    result.error = 'chatgpt_tab_not_found';
    return result;
  }
  result.chatgpt_tab_found = true;

  let pageResult;
  try {
    pageResult = await executePageGet(tab.id, '/backend-api/me', {});
    result.main_world_execution = true;
  } catch (error) {
    result.error = error instanceof Error ? error.message : 'main_world_execution_failed';
    return result;
  }

  if (!pageResult || typeof pageResult !== 'object') {
    result.error = 'main_world_invalid_result';
    return result;
  }

  const accountId = await accountContextForTab(tab.id);
  result.account_context = accountId !== null;
  result.http_status = pageResult.http_status;
  result.content_type = pageResult.content_type;
  const status = Number.isInteger(pageResult.http_status) ? pageResult.http_status : null;
  result.authentication = status === 200
    ? 'authenticated'
    : (status === 401 || status === 403)
      ? 'unauthenticated'
      : 'unknown';

  if (status === 200 || status === 401 || status === 403) {
    result.ok = true;
    result.body = pageResult.body;
  } else {
    result.error = typeof pageResult.error === 'string'
      ? pageResult.error
      : 'remote_fetch_failed';
  }
  return result;
}

function decodeCdpBody(payload) {
  if (!payload || typeof payload.body !== 'string') return null;
  if (payload.base64Encoded === true) {
    try {
      return atob(payload.body);
    } catch {
      return null;
    }
  }
  return payload.body;
}

async function captureCandidateBody(session, requestId) {
  const meta = session.pendingCandidates.get(requestId);
  if (!meta || session.closed) return;
  session.pendingCandidates.delete(requestId);

  const task = (async () => {
    let payload;
    try {
      payload = await chrome.debugger.sendCommand(
        session.debuggee,
        'Network.getResponseBody',
        { requestId },
      );
    } catch {
      session.body_read_failures += 1;
      return;
    }

    const text = decodeCdpBody(payload);
    if (text === null) {
      session.body_read_failures += 1;
      return;
    }
    if (new TextEncoder().encode(text).byteLength > MAX_RESPONSE_BYTES) {
      session.body_too_large += 1;
      return;
    }

    let body;
    try {
      body = JSON.parse(text);
    } catch {
      session.invalid_json += 1;
      return;
    }

    const classified = classifyHistoryBody(meta.url, body);
    if (classified !== null) session.candidates.push(classified);
  })();

  session.bodyTasks.add(task);
  try {
    await task;
  } finally {
    session.bodyTasks.delete(task);
  }
}

chrome.debugger.onEvent.addListener((source, method, params) => {
  const tabId = source?.tabId;
  if (!Number.isInteger(tabId)) return;
  const session = activeDiscoveries.get(tabId);
  if (!session || session.closed) return;

  if (method === 'Network.requestWillBeSent' || method === 'Network.requestWillBeSentExtraInfo') {
    const headers = method === 'Network.requestWillBeSent'
      ? params?.request?.headers
      : params?.headers;
    const accountId = headerValue(headers, ACCOUNT_HEADER);
    if (validAccountId(accountId)) {
      session.accountId = accountId;
      void rememberAccountContext(tabId, accountId);
    }
    return;
  }

  if (method === 'Network.responseReceived') {
    session.responses_seen += 1;
    const response = params?.response;
    if (!response || typeof response !== 'object') return;
    const backendUrl = safeBackendUrl(response.url);
    if (backendUrl !== null && response.status === 200) session.backend_200_seen += 1;
    if (!isCandidateResponse(response)) return;
    session.json_candidates_seen += 1;
    session.pendingCandidates.set(params.requestId, {
      url: response.url,
      mime_type: response.mimeType ?? '',
      status: response.status,
    });
    return;
  }

  if (method === 'Network.loadingFinished') {
    const requestId = params?.requestId;
    if (typeof requestId !== 'string') return;
    if (!session.pendingCandidates.has(requestId)) return;
    if (Number(params.encodedDataLength) > MAX_RESPONSE_BYTES) {
      session.pendingCandidates.delete(requestId);
      session.body_too_large += 1;
      return;
    }
    void captureCandidateBody(session, requestId);
  }
});

chrome.debugger.onDetach.addListener((source, reason) => {
  const tabId = source?.tabId;
  if (!Number.isInteger(tabId)) return;
  const session = activeDiscoveries.get(tabId);
  if (!session) return;
  session.detached_reason = typeof reason === 'string' ? reason : 'unknown';
});

chrome.debugger.onEvent.addListener((source, method, params) => {
  const tabId = source?.tabId;
  if (!Number.isInteger(tabId)) return;
  const capture = activeConversationCaptures.get(tabId);
  if (!capture || capture.closed) return;

  if (method === 'Network.responseReceived') {
    capture.responses_seen += 1;
    const response = params?.response;
    const matched = matchConversationResponse(response, capture.remoteId);
    if (matched !== null) {
      capture.exact_response_seen = true;
      capture.exact_response_count += 1;

      const disposition = classifyConversationHttpStatus(matched.http_status);
      if (disposition === 'transient_rate_limit') {
        capture.rate_limited_responses += 1;
        capture.lastRateLimitMeta = matched;
        void scheduleRateLimitReload(capture);
        return;
      }

      capture.responseMeta = matched;
      if (disposition === 'terminal_http_error') {
        capture.resolve?.('http-status');
      } else if (!isJsonMimeType(matched.mime_type)) {
        capture.non_json_response = true;
        capture.resolve?.('non-json');
      } else {
        capture.pendingRequestId = params.requestId;
      }
    }
    return;
  }

  if (method === 'Network.loadingFinished') {
    const requestId = params?.requestId;
    if (typeof requestId !== 'string' || capture.pendingRequestId !== requestId) return;
    capture.pendingRequestId = null;
    if (Number(params.encodedDataLength) > MAX_RESPONSE_BYTES) {
      capture.body_too_large += 1;
      capture.resolve?.('body-too-large');
      return;
    }
    void captureExactConversationBody(capture, requestId);
  }
});

chrome.debugger.onDetach.addListener((source, reason) => {
  const tabId = source?.tabId;
  if (!Number.isInteger(tabId)) return;
  const capture = activeConversationCaptures.get(tabId);
  if (!capture) return;
  capture.detached_reason = typeof reason === 'string' ? reason : 'unknown';
  capture.resolve?.('detached');
});

async function discoverHistorySurfaces(command) {
  const result = baseResult(command, DISCOVERY_PROFILE);
  const tab = await findChatGptTab();
  if (!tab) {
    result.error = 'chatgpt_tab_not_found';
    return result;
  }
  result.chatgpt_tab_found = true;

  const debuggee = { tabId: tab.id };
  const session = {
    debuggee,
    tabId: tab.id,
    closed: false,
    accountId: null,
    responses_seen: 0,
    backend_200_seen: 0,
    json_candidates_seen: 0,
    body_read_failures: 0,
    body_too_large: 0,
    invalid_json: 0,
    detached_reason: null,
    pendingCandidates: new Map(),
    bodyTasks: new Set(),
    candidates: [],
  };

  if (activeDiscoveries.has(tab.id)) {
    result.error = 'history_discovery_already_active';
    return result;
  }
  activeDiscoveries.set(tab.id, session);

  let attached = false;
  try {
    await chrome.debugger.attach(debuggee, DEBUGGER_PROTOCOL_VERSION);
    attached = true;
    result.debugger_attached = true;

    await chrome.debugger.sendCommand(debuggee, 'Network.enable', {
      maxTotalBufferSize: 16 * 1024 * 1024,
      maxResourceBufferSize: MAX_RESPONSE_BYTES,
      maxPostDataSize: 0,
    });
    result.network_enabled = true;

    await chrome.tabs.reload(tab.id);
    result.reload_started = true;

    await sleep(DISCOVERY_WINDOW_MS);
    await sleep(DISCOVERY_BODY_GRACE_MS);
    if (session.bodyTasks.size > 0) {
      await Promise.allSettled([...session.bodyTasks]);
    }
  } catch (error) {
    result.error = error instanceof Error ? error.message : 'cdp_discovery_failed';
  } finally {
    session.closed = true;
    activeDiscoveries.delete(tab.id);
    if (attached) {
      try {
        await chrome.debugger.detach(debuggee);
      } catch {
        // The browser may already have detached us during navigation/shutdown.
      }
    }
  }

  const accountId = session.accountId ?? await accountContextForTab(tab.id);
  result.account_context = accountId !== null;
  result.responses_seen = session.responses_seen;
  result.backend_http_200_seen = session.backend_200_seen;
  result.json_candidates_seen = session.json_candidates_seen;
  result.body_read_failures = session.body_read_failures;
  result.body_too_large = session.body_too_large;
  result.invalid_json = session.invalid_json;
  result.detached_reason = session.detached_reason;

  const candidates = mergeDiscoveryCandidates(session.candidates);
  result.candidate_count = candidates.length;
  result.candidates = candidates;

  if (result.error) return result;
  if (!result.debugger_attached || !result.network_enabled || !result.reload_started) {
    result.error = 'cdp_proof_incomplete';
    return result;
  }
  if (session.responses_seen === 0) {
    result.error = 'cdp_no_network_responses';
    return result;
  }

  result.ok = true;
  result.discovery = candidates.length > 0 ? 'candidates_observed' : 'no_candidate_surface_observed';
  return result;
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

async function captureExactConversationBody(session, requestId) {
  if (session.closed || session.bodyTask !== null) return;

  const task = (async () => {
    let payload;
    try {
      payload = await chrome.debugger.sendCommand(
        session.debuggee,
        'Network.getResponseBody',
        { requestId },
      );
    } catch {
      session.body_read_failures += 1;
      session.resolve?.('body-read-failed');
      return;
    }

    const text = decodeCdpBody(payload);
    if (text === null) {
      session.body_read_failures += 1;
      session.resolve?.('body-decode-failed');
      return;
    }
    if (new TextEncoder().encode(text).byteLength > MAX_RESPONSE_BYTES) {
      session.body_too_large += 1;
      session.resolve?.('body-too-large');
      return;
    }

    let body;
    try {
      body = JSON.parse(text);
    } catch {
      session.invalid_json += 1;
      session.resolve?.('invalid-json');
      return;
    }

    session.body = body;
    session.resolve?.('captured');
  })();

  session.bodyTask = task;
  try {
    await task;
  } finally {
    session.bodyTask = null;
  }
}

async function scheduleRateLimitReload(capture) {
  if (capture.rate_limit_reload_scheduled) return;
  capture.rate_limit_reload_scheduled = true;

  await sleep(CONVERSATION_RATE_LIMIT_RELOAD_DELAY_MS);
  if (
    capture.closed
    || capture.body !== null
    || capture.responseMeta !== null
  ) {
    return;
  }

  try {
    await chrome.debugger.sendCommand(capture.debuggee, 'Page.reload', {
      ignoreCache: false,
    });
    capture.rate_limit_reload_count += 1;
  } catch {
    capture.rate_limit_reload_failures += 1;
  }
}

async function captureConversationByNavigation(command, remoteId) {
  const result = baseResult(command, FETCH_PROFILE);
  result.capture_tab_created = false;
  result.navigation_started = false;
  result.exact_response_seen = false;
  result.exact_response_count = 0;
  result.rate_limited_responses = 0;
  result.rate_limit_reload_count = 0;
  result.rate_limit_reload_failures = 0;
  result.responses_seen = 0;
  result.body_read_failures = 0;
  result.body_too_large = 0;
  result.invalid_json = 0;
  result.detached_reason = null;

  const sourceTab = await findChatGptTab();
  if (!sourceTab) {
    result.error = 'chatgpt_tab_not_found';
    return result;
  }
  result.chatgpt_tab_found = true;

  const accountId = await accountContextForTab(sourceTab.id);
  if (accountId === null) {
    result.error = 'account_context_unavailable_run_history_discovery_first';
    return result;
  }
  result.account_context = true;

  const route = conversationRoute(remoteId);
  if (route === null) {
    result.error = 'invalid_remote_conversation_id';
    return result;
  }

  let captureTab = null;
  let attached = false;
  let session = null;

  try {
    captureTab = await chrome.tabs.create({
      active: false,
      url: 'about:blank',
      ...(Number.isInteger(sourceTab.windowId) ? { windowId: sourceTab.windowId } : {}),
    });
    if (!captureTab || !Number.isInteger(captureTab.id)) {
      result.error = 'capture_tab_creation_failed';
      return result;
    }
    result.capture_tab_created = true;

    let resolveCapture;
    const captureDone = new Promise((resolve) => {
      resolveCapture = resolve;
    });

    const debuggee = { tabId: captureTab.id };
    session = {
      debuggee,
      tabId: captureTab.id,
      remoteId,
      closed: false,
      resolve: resolveCapture,
      responses_seen: 0,
      exact_response_seen: false,
      exact_response_count: 0,
      rate_limited_responses: 0,
      rate_limit_reload_scheduled: false,
      rate_limit_reload_count: 0,
      rate_limit_reload_failures: 0,
      responseMeta: null,
      lastRateLimitMeta: null,
      pendingRequestId: null,
      bodyTask: null,
      body: null,
      non_json_response: false,
      body_read_failures: 0,
      body_too_large: 0,
      invalid_json: 0,
      detached_reason: null,
    };
    activeConversationCaptures.set(captureTab.id, session);

    await chrome.debugger.attach(debuggee, DEBUGGER_PROTOCOL_VERSION);
    attached = true;
    result.debugger_attached = true;

    await chrome.debugger.sendCommand(debuggee, 'Network.enable', {
      maxTotalBufferSize: 16 * 1024 * 1024,
      maxResourceBufferSize: MAX_RESPONSE_BYTES,
      maxPostDataSize: 0,
    });
    result.network_enabled = true;

    await chrome.debugger.sendCommand(debuggee, 'Page.enable');
    await chrome.debugger.sendCommand(debuggee, 'Page.navigate', { url: route });
    result.navigation_started = true;

    const outcome = await Promise.race([
      captureDone,
      sleep(CONVERSATION_CAPTURE_WINDOW_MS).then(() => 'timeout'),
    ]);

    await settleWithin(
      session.bodyTask === null ? [] : [session.bodyTask],
      CONVERSATION_CAPTURE_BODY_GRACE_MS,
    );

    result.responses_seen = session.responses_seen;
    result.exact_response_seen = session.exact_response_seen;
    result.exact_response_count = session.exact_response_count;
    result.rate_limited_responses = session.rate_limited_responses;
    result.rate_limit_reload_count = session.rate_limit_reload_count;
    result.rate_limit_reload_failures = session.rate_limit_reload_failures;
    result.body_read_failures = session.body_read_failures;
    result.body_too_large = session.body_too_large;
    result.invalid_json = session.invalid_json;
    result.detached_reason = session.detached_reason;

    const finalResponseMeta = selectFinalConversationResponseMeta(
      session.responseMeta,
      session.lastRateLimitMeta,
    );
    if (finalResponseMeta !== null) {
      result.first_party_http_status = finalResponseMeta.http_status;
      result.http_status = finalResponseMeta.http_status;
      result.content_type = finalResponseMeta.mime_type;
    }

    if (outcome === 'timeout') {
      if (
        session.body === null
        && session.responseMeta === null
        && session.lastRateLimitMeta !== null
      ) {
        result.error = 'exact_conversation_rate_limited';
      } else {
        result.error = session.exact_response_seen
          ? 'exact_conversation_body_not_completed_before_timeout'
          : 'exact_conversation_response_not_observed_before_timeout';
      }
      return result;
    }
    if (session.responseMeta !== null && session.responseMeta.http_status !== 200) {
      result.error = 'first_party_conversation_http_status';
      return result;
    }
    if (session.non_json_response) {
      result.error = 'first_party_conversation_non_json';
      return result;
    }
    if (session.body_too_large > 0) {
      result.error = 'first_party_conversation_body_too_large';
      return result;
    }
    if (session.invalid_json > 0) {
      result.error = 'first_party_conversation_invalid_json';
      return result;
    }
    if (session.body_read_failures > 0 || session.body === null) {
      result.error = 'first_party_conversation_body_unavailable';
      return result;
    }

    result.ok = true;
    result.body = session.body;
    return result;
  } catch (error) {
    result.error = error instanceof Error ? error.message : 'cdp_conversation_capture_failed';
    return result;
  } finally {
    if (session !== null) {
      session.closed = true;
      if (Number.isInteger(session.tabId)) activeConversationCaptures.delete(session.tabId);
    }
    if (attached && captureTab && Number.isInteger(captureTab.id)) {
      try {
        await chrome.debugger.detach({ tabId: captureTab.id });
      } catch {
        // Navigation, browser shutdown, or tab teardown may already have detached the debugger.
      }
    }
    if (captureTab && Number.isInteger(captureTab.id)) {
      try {
        await chrome.tabs.remove(captureTab.id);
      } catch {
        // The temporary capture tab may already be gone.
      }
    }
  }
}

async function fetchConversation(command) {
  const remoteId = typeof command.remote_conversation_id === 'string'
    ? command.remote_conversation_id
    : '';
  if (!remoteId) {
    const result = baseResult(command, FETCH_PROFILE);
    result.error = 'missing_remote_conversation_id';
    return result;
  }

  const expectedResource =
    `/backend-api/conversations/${percentEncodePathSegment(remoteId)}?num_turns=10&include_has_versions=true`;
  if (command.resource !== expectedResource || command.request_profile !== FETCH_PROFILE) {
    const result = baseResult(command, FETCH_PROFILE);
    result.error = 'resource_profile_mismatch';
    return result;
  }

  return captureConversationByNavigation(command, remoteId);
}

async function execute(command) {
  if (!command || command.version !== PROTOCOL_VERSION || typeof command.id !== 'string') {
    return null;
  }

  switch (command.kind) {
    case 'probe_auth':
      return probeAuthentication(command);
    case 'discover_history_surfaces':
      return discoverHistorySurfaces(command);
    case 'discover_history_surfaces_fresh_tab':
      return discoverHistorySurfacesFreshTab(command);
    case 'fetch_conversation':
      return fetchConversation(command);
    case 'list_conversations':
      return {
        ...baseResult(command, DISCOVERY_PROFILE),
        error: 'retired_exact_c01_replay_use_discover_history_surfaces',
      };
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
      if (!result) throw new Error('loopback_invalid_command');

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

function installServiceWorkerKeepalive() {
  setInterval(() => {
    // Chromium MV3 workers are normally retired after 30s of inactivity.
    // This local bridge is intentionally long-lived while Edge is running:
    // a harmless extension API call resets the MV3 idle timer without
    // generating any ChatGPT/OpenAI network traffic.
    void chrome.runtime.getPlatformInfo().catch(() => {});
  }, SERVICE_WORKER_KEEPALIVE_MS);
}

chrome.runtime.onInstalled.addListener(ensureBridgeLoop);
chrome.runtime.onStartup.addListener(ensureBridgeLoop);
installServiceWorkerKeepalive();
ensureBridgeLoop();
