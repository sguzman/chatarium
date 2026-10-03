// ==UserScript==
// @name         Chatarium Account Bridge
// @namespace    https://github.com/sguzman/chatarium
// @version      0.2.0
// @description  Narrow credential-contained bridge from Chatarium Desktop to the authenticated ChatGPT web session.
// @match        https://chatgpt.com/*
// @run-at       document-start
// @grant        GM_xmlhttpRequest
// @grant        unsafeWindow
// @connect      127.0.0.1
// ==/UserScript==

(() => {
  'use strict';

  if (window.top !== window.self) return;

  const BRIDGE_ORIGIN = 'http://127.0.0.1:43117';
  const BRIDGE_HEADER = 'X-Chatarium-Bridge';
  const BRIDGE_HEADER_VALUE = '1';
  const VERSION = 1;
  const LIST_RESOURCE =
    '/backend-api/conversations?exclude_conversation_origin=tpp&expand=false&hide_snorlax=false&is_archived=false&is_starred=false&limit=20&order=updated&offset=0';
  const MAX_RESPONSE_BYTES = 4 * 1024 * 1024;
  const ERROR_RETRY_MS = 750;

  function sleep(ms) {
    return new Promise((resolve) => setTimeout(resolve, ms));
  }

  function loopbackRequest(method, path, body = null, timeout = 30_000) {
    return new Promise((resolve, reject) => {
      GM_xmlhttpRequest({
        method,
        url: `${BRIDGE_ORIGIN}${path}`,
        headers: {
          [BRIDGE_HEADER]: BRIDGE_HEADER_VALUE,
          ...(body === null ? {} : { 'Content-Type': 'application/json' }),
        },
        data: body === null ? undefined : JSON.stringify(body),
        timeout,
        onload: resolve,
        onerror: () => reject(new Error('loopback request failed')),
        ontimeout: () => reject(new Error('loopback request timed out')),
        onabort: () => reject(new Error('loopback request aborted')),
      });
    });
  }

  function parseLoopbackJson(response) {
    try {
      return JSON.parse(response.responseText);
    } catch {
      throw new Error('loopback returned invalid JSON');
    }
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

  async function readResponseTextBounded(response) {
    const declaredLength = Number(response.headers.get('content-length'));
    if (Number.isFinite(declaredLength) && declaredLength > MAX_RESPONSE_BYTES) {
      throw new Error('conversation response exceeds bridge byte limit');
    }

    if (!response.body) {
      const text = await response.text();
      if (new TextEncoder().encode(text).byteLength > MAX_RESPONSE_BYTES) {
        throw new Error('conversation response exceeds bridge byte limit');
      }
      return text;
    }

    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let byteCount = 0;
    let text = '';
    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        byteCount += value.byteLength;
        if (byteCount > MAX_RESPONSE_BYTES) {
          await reader.cancel('Chatarium bridge byte limit');
          throw new Error('conversation response exceeds bridge byte limit');
        }
        text += decoder.decode(value, { stream: true });
      }
      text += decoder.decode();
      return text;
    } finally {
      reader.releaseLock();
    }
  }

  async function sameOriginFetch(resource) {
    const pageFetch = unsafeWindow.fetch.bind(unsafeWindow);
    return pageFetch(resource, {
      method: 'GET',
      credentials: 'include',
      cache: 'no-store',
      redirect: 'error',
    });
  }

  async function probeAuthentication(command) {
    try {
      const response = await sameOriginFetch('/backend-api/me');
      const state = response.status === 200
        ? 'authenticated'
        : (response.status === 401 || response.status === 403)
          ? 'unauthenticated'
          : 'unknown';
      return {
        version: VERSION,
        id: command.id,
        kind: command.kind,
        ok: true,
        authentication: state,
        http_status: response.status,
      };
    } catch {
      return {
        version: VERSION,
        id: command.id,
        kind: command.kind,
        ok: false,
        authentication: 'unknown',
        error: 'authentication_probe_failed',
      };
    }
  }

  async function listConversations(command) {
    if (command.resource !== LIST_RESOURCE) {
      return {
        version: VERSION,
        id: command.id,
        kind: command.kind,
        ok: false,
        error: 'resource_profile_mismatch',
      };
    }

    try {
      const response = await sameOriginFetch(LIST_RESOURCE);
      const contentType = response.headers.get('content-type') ?? '';

      if (response.status !== 200) {
        return {
          version: VERSION,
          id: command.id,
          kind: command.kind,
          ok: false,
          http_status: response.status,
          content_type: contentType,
          error: 'remote_http_status',
        };
      }

      if (!contentType.toLowerCase().split(';', 1)[0].trim().endsWith('json')) {
        return {
          version: VERSION,
          id: command.id,
          kind: command.kind,
          ok: false,
          http_status: response.status,
          content_type: contentType,
          error: 'remote_non_json_response',
        };
      }

      const text = await readResponseTextBounded(response);
      let body;
      try {
        body = JSON.parse(text);
      } catch {
        return {
          version: VERSION,
          id: command.id,
          kind: command.kind,
          ok: false,
          http_status: response.status,
          content_type: contentType,
          error: 'remote_invalid_json',
        };
      }

      return {
        version: VERSION,
        id: command.id,
        kind: command.kind,
        ok: true,
        http_status: response.status,
        content_type: contentType,
        body,
      };
    } catch (error) {
      return {
        version: VERSION,
        id: command.id,
        kind: command.kind,
        ok: false,
        error: error instanceof Error ? error.message : 'remote_fetch_failed',
      };
    }
  }

  async function fetchConversation(command) {
    const remoteId = typeof command.remote_conversation_id === 'string'
      ? command.remote_conversation_id
      : '';
    if (!remoteId) {
      return {
        version: VERSION,
        id: command.id,
        kind: command.kind,
        ok: false,
        error: 'missing_remote_conversation_id',
      };
    }

    const expectedResource =
      `/backend-api/conversations/${percentEncodePathSegment(remoteId)}?num_turns=10&include_has_versions=true`;
    if (command.resource !== expectedResource) {
      return {
        version: VERSION,
        id: command.id,
        kind: command.kind,
        ok: false,
        error: 'resource_profile_mismatch',
      };
    }
    const resource = expectedResource;

    try {
      const response = await sameOriginFetch(resource);
      const contentType = response.headers.get('content-type') ?? '';

      if (response.status !== 200) {
        return {
          version: VERSION,
          id: command.id,
          kind: command.kind,
          ok: false,
          http_status: response.status,
          content_type: contentType,
          error: 'remote_http_status',
        };
      }

      if (!contentType.toLowerCase().split(';', 1)[0].trim().endsWith('json')) {
        return {
          version: VERSION,
          id: command.id,
          kind: command.kind,
          ok: false,
          http_status: response.status,
          content_type: contentType,
          error: 'remote_non_json_response',
        };
      }

      const text = await readResponseTextBounded(response);
      let body;
      try {
        body = JSON.parse(text);
      } catch {
        return {
          version: VERSION,
          id: command.id,
          kind: command.kind,
          ok: false,
          http_status: response.status,
          content_type: contentType,
          error: 'remote_invalid_json',
        };
      }

      return {
        version: VERSION,
        id: command.id,
        kind: command.kind,
        ok: true,
        http_status: response.status,
        content_type: contentType,
        body,
      };
    } catch (error) {
      return {
        version: VERSION,
        id: command.id,
        kind: command.kind,
        ok: false,
        error: error instanceof Error ? error.message : 'remote_fetch_failed',
      };
    }
  }

  async function execute(command) {
    if (!command || command.version !== VERSION || typeof command.id !== 'string') {
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
          version: VERSION,
          id: command.id,
          kind: String(command.kind ?? ''),
          ok: false,
          error: 'unsupported_command',
        };
    }
  }

  async function main() {
    for (;;) {
      try {
        const response = await loopbackRequest('GET', '/v1/next', null, 30_000);
        if (response.status === 204) continue;
        if (response.status !== 200) {
          await sleep(ERROR_RETRY_MS);
          continue;
        }

        const command = parseLoopbackJson(response);
        const result = await execute(command);
        if (!result) {
          await sleep(ERROR_RETRY_MS);
          continue;
        }

        const posted = await loopbackRequest('POST', '/v1/result', result, 10_000);
        if (posted.status !== 204) {
          await sleep(ERROR_RETRY_MS);
        }
      } catch {
        await sleep(ERROR_RETRY_MS);
      }
    }
  }

  void main();
})();
