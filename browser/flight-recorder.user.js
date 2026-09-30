// ==UserScript==
// @name         Chatarium Flight Recorder
// @namespace    https://github.com/sguzman/chatarium
// @version      0.7.0
// @description  Local durability layer for ChatGPT drafts, send intents, assistant output, and visible failures.
// @match        https://chatgpt.com/*
// @run-at       document-start
// @updateURL    https://raw.githubusercontent.com/sguzman/chatarium/main/browser/flight-recorder.user.js
// @downloadURL  https://raw.githubusercontent.com/sguzman/chatarium/main/browser/flight-recorder.user.js
// @grant        none
// ==/UserScript==

(() => {
  'use strict';

  // ChatGPT loads same-origin helper/sentinel frames. They are evidence sources for the site, not
  // independent Chatarium surfaces. Running the recorder in them polluted the event stream.
  if (window.top !== window.self) return;

  const VERSION = '0.7.0';
  const DB_NAME = 'chatarium-flight-recorder';
  const DB_VERSION = 1;
  const DRAFT_WAL_PREFIX = 'chatarium:p0:draft-wal:';
  const SEND_WAL_KEY = 'chatarium:p0:send-intents';
  const ASSISTANT_WAL_KEY = 'chatarium:p0:assistant-wal';
  const ERROR_WAL_KEY = 'chatarium:p0:last-visible-error';
  const LEGACY_WAL_KEY = 'chatarium:p0:wal';
  const MAX_SEND_INTENTS = 50;
  const MAX_ASSISTANT_WAL_CHARS = 500_000;
  const MAX_ERROR_WAL_CHARS = 5_000;
  const MAX_NETWORK_STREAM_BYTES = 8_000_000;
  const MAX_READ_RESPONSE_BYTES = 1_000_000;
  const MAX_READ_RUN_BYTES = 4_000_000;
  const COMPOSER_POLL_MS = 250;
  const EXPORT_HOTKEY = { ctrlKey: true, shiftKey: true, altKey: true, code: 'KeyE' };

  let dbPromise;
  let lastHref = location.href;
  let transcriptTimer = 0;
  let draftTimer = 0;
  let statusRefreshTimer = 0;
  let panelHost = null;
  let panelRoot = null;
  let lastPolledComposerFingerprint = null;
  let readCaptureArmed = false;
  let readCaptureArmedAt = null;
  let readCaptureBytes = 0;
  let readCaptureResponses = 0;
  const readCaptureIntervals = [];
  const observedErrors = new Set();

  const now = () => new Date().toISOString();

  function conversationKey() {
    const match = location.pathname.match(/^\/c\/([^/?#]+)/);
    return match ? `conversation:${match[1]}` : `route:${location.pathname}${location.search}`;
  }

  function safeJsonParse(value, fallback) {
    if (!value) return fallback;
    try {
      return JSON.parse(value);
    } catch {
      return fallback;
    }
  }

  function normalizeText(value) {
    return String(value ?? '')
      .replace(/\r\n/g, '\n')
      .replace(/[ \t]+\n/g, '\n')
      .trim();
  }

  function makeId(prefix) {
    if (globalThis.crypto?.randomUUID) return `${prefix}:${crypto.randomUUID()}`;
    return `${prefix}:${Date.now()}:${Math.random().toString(16).slice(2)}`;
  }

  function writeLocalJson(key, value, label) {
    try {
      localStorage.setItem(key, JSON.stringify(value));
      return true;
    } catch (error) {
      console.error(`[chatarium] ${label} write failed`, error);
      return false;
    }
  }

  function openDb() {
    if (dbPromise) return dbPromise;

    dbPromise = new Promise((resolve, reject) => {
      const request = indexedDB.open(DB_NAME, DB_VERSION);
      request.onupgradeneeded = () => {
        const db = request.result;
        if (!db.objectStoreNames.contains('events')) {
          const events = db.createObjectStore('events', { keyPath: 'seq', autoIncrement: true });
          events.createIndex('conversation', 'conversation', { unique: false });
          events.createIndex('at', 'at', { unique: false });
        }
        if (!db.objectStoreNames.contains('drafts')) {
          db.createObjectStore('drafts', { keyPath: 'conversation' });
        }
        if (!db.objectStoreNames.contains('messages')) {
          const messages = db.createObjectStore('messages', { keyPath: 'key' });
          messages.createIndex('conversation', 'conversation', { unique: false });
        }
      };
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });

    return dbPromise;
  }

  async function tx(storeName, mode, operation) {
    const db = await openDb();
    return new Promise((resolve, reject) => {
      const transaction = db.transaction(storeName, mode);
      const store = transaction.objectStore(storeName);
      let result;
      try {
        result = operation(store);
      } catch (error) {
        reject(error);
        return;
      }
      transaction.oncomplete = () => resolve(result);
      transaction.onerror = () => reject(transaction.error);
      transaction.onabort = () => reject(transaction.error ?? new Error('transaction aborted'));
    });
  }

  function appendEvent(kind, payload = {}) {
    const event = {
      at: now(),
      href: location.href,
      conversation: conversationKey(),
      kind,
      payload,
    };
    void tx('events', 'readwrite', (store) => store.add(event)).catch((error) => {
      console.error('[chatarium] event write failed', error);
    });
  }

  function networkRequestInfo(input, init) {
    try {
      const requestUrl = input instanceof Request
        ? input.url
        : input instanceof URL
          ? input.href
          : String(input);
      const url = new URL(requestUrl, location.href);
      const method = String(init?.method ?? (input instanceof Request ? input.method : 'GET')).toUpperCase();
      const sameOriginBackend = url.origin === location.origin && url.pathname.startsWith('/backend-api/');
      return {
        url,
        method,
        queryKeys: [...new Set([...url.searchParams.keys()])].sort(),
        captureStream: method === 'POST'
          && url.origin === location.origin
          && url.pathname === '/backend-api/f/conversation',
        readEligible: (method === 'GET' || method === 'HEAD') && sameOriginBackend,
      };
    } catch {
      return {
        url: null,
        method: null,
        queryKeys: [],
        captureStream: false,
        readEligible: false,
      };
    }
  }

  function structuredReadContentType(contentType) {
    const mime = String(contentType ?? '').split(';')[0].trim().toLowerCase();
    return mime === 'application/json'
      || mime === 'text/json'
      || mime.endsWith('+json');
  }

  function readCaptureStatus() {
    return {
      armed: readCaptureArmed,
      armedAt: readCaptureArmedAt,
      capturedBytes: readCaptureBytes,
      responseCount: readCaptureResponses,
      responseLimitBytes: MAX_READ_RESPONSE_BYTES,
      runLimitBytes: MAX_READ_RUN_BYTES,
      intervals: readCaptureIntervals.map((interval) => ({ ...interval })),
    };
  }

  function armReadCapture() {
    if (readCaptureArmed) return readCaptureStatus();
    readCaptureArmed = true;
    readCaptureArmedAt = now();
    readCaptureBytes = 0;
    readCaptureResponses = 0;
    readCaptureIntervals.push({
      armedAt: readCaptureArmedAt,
      disarmedAt: null,
      reason: null,
    });
    appendEvent('protocol-read-capture-armed', {
      responseLimitBytes: MAX_READ_RESPONSE_BYTES,
      runLimitBytes: MAX_READ_RUN_BYTES,
    });
    scheduleStatusRefresh();
    return readCaptureStatus();
  }

  function disarmReadCapture(reason = 'operator') {
    if (!readCaptureArmed) return readCaptureStatus();
    readCaptureArmed = false;
    const disarmedAt = now();
    const interval = readCaptureIntervals.at(-1);
    if (interval && interval.disarmedAt === null) {
      interval.disarmedAt = disarmedAt;
      interval.reason = reason;
    }
    appendEvent('protocol-read-capture-disarmed', {
      reason,
      capturedBytes: readCaptureBytes,
      responseCount: readCaptureResponses,
    });
    readCaptureArmedAt = null;
    scheduleStatusRefresh();
    return readCaptureStatus();
  }

  function conversationScopeFromProtocolId(conversationId) {
    return conversationId ? `conversation:${conversationId}` : conversationKey();
  }

  function protocolMessageText(message) {
    const parts = message?.content?.parts;
    if (!Array.isArray(parts)) return '';
    return parts.filter((part) => typeof part === 'string').join('');
  }

  function archiveProtocolMessage(message, conversationId, extra = {}) {
    if (!message?.id || !message?.author?.role) return;
    const text = protocolMessageText(message);
    if (!text && message.author.role !== 'assistant') return;

    const conversation = conversationScopeFromProtocolId(conversationId);
    const record = {
      key: `${conversation}:id:${message.id}`,
      conversation,
      href: location.href,
      observedAt: now(),
      observedId: message.id,
      role: message.author.role,
      index: null,
      contentHash: fallbackHash(text),
      transientStatus: message.status !== 'finished_successfully',
      text,
      source: 'protocol-sse',
      protocolStatus: message.status ?? null,
      protocolEndTurn: message.end_turn ?? null,
      ...extra,
    };

    void tx('messages', 'readwrite', (store) => store.put(record)).catch((error) => {
      console.error('[chatarium] protocol message archive write failed', error);
    });

    if (message.author.role === 'assistant' && message.channel === 'final') {
      writeAssistantWal(record);
    }
  }

  function createProtocolStreamState(streamId) {
    return {
      streamId,
      sseBuffer: '',
      frameIndex: 0,
      deltaEncoding: null,
      conversationId: null,
      assistant: null,
      completion: {
        finishedSuccessfully: false,
        endTurn: false,
        isComplete: false,
        messageStreamComplete: false,
        done: false,
      },
    };
  }

  function observeProtocolUserInput(state, payload) {
    const message = payload?.input_message;
    if (message?.author?.role !== 'user') return;

    const text = normalizeText(protocolMessageText(message));
    if (!text) return;

    const conversationId = payload?.conversation_id ?? state.conversationId ?? null;
    if (conversationId) state.conversationId = conversationId;
    const conversation = conversationScopeFromProtocolId(conversationId);

    confirmObservedUserMessage(text, message.id ?? null, {
      conversation,
      evidence: 'protocol-input-message',
    });
    archiveProtocolMessage(message, conversationId, {
      protocolEvidence: 'input_message',
    });
    appendEvent('protocol-user-message-observed', {
      streamId: state.streamId,
      observedMessageId: message.id ?? null,
      conversation,
      contentHash: fallbackHash(text),
    });
  }

  function observeProtocolAssistantMessage(state, envelope) {
    const message = envelope?.message;
    if (
      message?.author?.role !== 'assistant'
      || message?.channel !== 'final'
      || message?.content?.content_type !== 'text'
      || !message.id
    ) {
      return;
    }

    const conversationId = envelope?.conversation_id ?? state.conversationId ?? null;
    if (conversationId) state.conversationId = conversationId;

    state.assistant = {
      id: message.id,
      conversationId,
      text: protocolMessageText(message),
      status: message.status ?? null,
      endTurn: message.end_turn ?? null,
      metadata: { ...(message.metadata ?? {}) },
    };

    archiveProtocolMessage(
      {
        ...message,
        content: {
          ...message.content,
          parts: [state.assistant.text],
        },
      },
      conversationId,
      { protocolEvidence: 'final-assistant-message' },
    );
    appendEvent('protocol-assistant-message-observed', {
      streamId: state.streamId,
      observedMessageId: message.id,
      conversation: conversationScopeFromProtocolId(conversationId),
      status: state.assistant.status,
      endTurn: state.assistant.endTurn,
      contentHash: fallbackHash(state.assistant.text),
    });
  }

  function persistProtocolAssistantState(state, evidence) {
    const assistant = state.assistant;
    if (!assistant?.id) return;

    const message = {
      id: assistant.id,
      author: { role: 'assistant' },
      content: { content_type: 'text', parts: [assistant.text] },
      status: assistant.status,
      end_turn: assistant.endTurn,
      metadata: assistant.metadata,
      channel: 'final',
    };

    archiveProtocolMessage(message, assistant.conversationId ?? state.conversationId, {
      protocolEvidence: evidence,
      protocolIsComplete: Boolean(assistant.metadata?.is_complete),
    });
  }

  function observeProtocolDelta(state, payload) {
    if (!payload || typeof payload !== 'object') return;

    if (payload?.v?.conversation_id) state.conversationId = payload.v.conversation_id;
    if (payload?.v?.message) observeProtocolAssistantMessage(state, payload.v);

    if (!state.assistant) return;

    if (
      payload.o === 'append'
      && payload.p === '/message/content/parts/0'
      && typeof payload.v === 'string'
    ) {
      state.assistant.text += payload.v;
      persistProtocolAssistantState(state, 'delta-append');
      appendEvent('protocol-assistant-text-appended', {
        streamId: state.streamId,
        observedMessageId: state.assistant.id,
        appendedChars: payload.v.length,
        totalChars: state.assistant.text.length,
      });
      return;
    }

    if (payload.o === 'patch' && Array.isArray(payload.v)) {
      const evidence = {};
      for (const operation of payload.v) {
        if (!operation || typeof operation !== 'object') continue;
        if (operation.p === '/message/status' && operation.o === 'replace') {
          state.assistant.status = operation.v;
          evidence.status = operation.v;
          if (operation.v === 'finished_successfully') {
            state.completion.finishedSuccessfully = true;
          }
        } else if (operation.p === '/message/end_turn' && operation.o === 'replace') {
          state.assistant.endTurn = operation.v;
          evidence.endTurn = operation.v;
          if (operation.v === true) state.completion.endTurn = true;
        } else if (
          operation.p === '/message/metadata'
          && operation.o === 'append'
          && operation.v
          && typeof operation.v === 'object'
        ) {
          Object.assign(state.assistant.metadata, operation.v);
          evidence.metadataKeys = Object.keys(operation.v);
          if (operation.v.is_complete === true) state.completion.isComplete = true;
        }
      }

      if (Object.keys(evidence).length) {
        persistProtocolAssistantState(state, 'delta-completion-patch');
        appendEvent('protocol-assistant-completion-patch', {
          streamId: state.streamId,
          observedMessageId: state.assistant.id,
          ...evidence,
        });
      }
    }
  }

  function observeProtocolControlFrame(state, payload) {
    if (!payload || typeof payload !== 'object') return;

    const conversationId = payload.conversation_id ?? null;
    if (conversationId) state.conversationId = conversationId;

    switch (payload.type) {
      case 'resume_conversation_token':
        appendEvent('protocol-conversation-identity-observed', {
          streamId: state.streamId,
          conversation: conversationScopeFromProtocolId(conversationId),
          kind: payload.kind ?? null,
        });
        break;
      case 'input_message':
        observeProtocolUserInput(state, payload);
        break;
      case 'message_marker':
        appendEvent('protocol-message-marker-observed', {
          streamId: state.streamId,
          conversation: conversationScopeFromProtocolId(conversationId),
          observedMessageId: payload.message_id ?? null,
          marker: payload.marker ?? null,
          event: payload.event ?? null,
        });
        break;
      case 'message_stream_complete':
        state.completion.messageStreamComplete = true;
        appendEvent('protocol-message-stream-complete', {
          streamId: state.streamId,
          conversation: conversationScopeFromProtocolId(conversationId),
          observedMessageId: state.assistant?.id ?? null,
        });
        break;
      case 'title_generation':
      case 'conversation_detail_metadata':
      case 'server_ste_metadata':
        appendEvent('protocol-control-frame-observed', {
          streamId: state.streamId,
          type: payload.type,
          conversation: conversationScopeFromProtocolId(conversationId),
        });
        break;
      default:
        appendEvent('protocol-unknown-control-frame-observed', {
          streamId: state.streamId,
          type: typeof payload.type === 'string' ? payload.type : null,
        });
        break;
    }
  }

  function observeProtocolSseFrame(state, eventName, data) {
    const frameIndex = state.frameIndex;
    state.frameIndex += 1;

    if (data === '[DONE]') {
      state.completion.done = true;
      appendEvent('protocol-sse-done', {
        streamId: state.streamId,
        frameIndex,
        completionEvidence: { ...state.completion },
        observedMessageId: state.assistant?.id ?? null,
      });
      return;
    }

    let payload;
    try {
      payload = JSON.parse(data);
    } catch (error) {
      appendEvent('protocol-sse-parse-error', {
        streamId: state.streamId,
        frameIndex,
        event: eventName,
        message: String(error?.message ?? error),
        dataChars: data.length,
      });
      return;
    }

    if (eventName === 'delta_encoding') {
      if (typeof payload === 'string') {
        state.deltaEncoding = payload;
        appendEvent('protocol-delta-encoding-observed', {
          streamId: state.streamId,
          frameIndex,
          encoding: payload,
        });
      }
      return;
    }

    if (eventName === 'delta') {
      observeProtocolDelta(state, payload);
      return;
    }

    observeProtocolControlFrame(state, payload);
  }

  function feedProtocolSseText(state, text) {
    state.sseBuffer = (state.sseBuffer + text).replace(/\r\n/g, '\n');

    while (true) {
      const boundary = state.sseBuffer.indexOf('\n\n');
      if (boundary < 0) break;

      const rawFrame = state.sseBuffer.slice(0, boundary);
      state.sseBuffer = state.sseBuffer.slice(boundary + 2);
      if (!rawFrame.trim()) continue;

      let eventName = null;
      const dataLines = [];
      for (const line of rawFrame.split('\n')) {
        if (!line || line.startsWith(':')) continue;
        if (line.startsWith('event:')) {
          eventName = line.slice('event:'.length).trim();
        } else if (line.startsWith('data:')) {
          const value = line.slice('data:'.length);
          dataLines.push(value.startsWith(' ') ? value.slice(1) : value);
        }
      }

      if (!dataLines.length) continue;
      observeProtocolSseFrame(state, eventName, dataLines.join('\n'));
    }
  }

  async function captureProtocolReadResponse(response, requestInfo) {
    const readId = makeId('protocol-read');
    const contentType = response.headers.get('content-type') ?? '';
    const endpoint = requestInfo.url?.pathname ?? '<unknown>';
    const metadata = {
      readId,
      method: requestInfo.method,
      endpoint,
      queryKeys: requestInfo.queryKeys,
      status: response.status,
      contentType,
      privateEvidence: true,
    };

    if (!structuredReadContentType(contentType)) {
      appendEvent('protocol-read-response-skipped', {
        ...metadata,
        reason: 'unsupported-content-type',
      });
      return;
    }

    if (requestInfo.method === 'HEAD') {
      readCaptureResponses += 1;
      appendEvent('protocol-read-response-captured', {
        ...metadata,
        capturedBytes: 0,
        truncated: false,
        bodyPresent: false,
        bodyText: null,
      });
      scheduleStatusRefresh();
      return;
    }

    if (!response.body) {
      appendEvent('protocol-read-response-error', {
        ...metadata,
        capturedBytes: 0,
        message: 'response-body-unavailable',
      });
      return;
    }

    const remainingRunBytes = Math.max(0, MAX_READ_RUN_BYTES - readCaptureBytes);
    if (remainingRunBytes === 0) {
      appendEvent('protocol-read-capture-limit', {
        limitBytes: MAX_READ_RUN_BYTES,
        capturedBytes: readCaptureBytes,
      });
      disarmReadCapture('run-byte-limit');
      return;
    }

    const limitBytes = Math.min(MAX_READ_RESPONSE_BYTES, remainingRunBytes);
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let capturedBytes = 0;
    let text = '';
    let truncated = false;

    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        if (!value?.byteLength) continue;

        const remaining = limitBytes - capturedBytes;
        if (value.byteLength > remaining) {
          if (remaining > 0) {
            const prefix = value.subarray(0, remaining);
            text += decoder.decode(prefix, { stream: true });
            capturedBytes += prefix.byteLength;
          }
          truncated = true;
          try {
            await reader.cancel('Chatarium protocol read capture limit reached');
          } catch {
            // Clone cancellation is diagnostic-only. The site's original response is untouched.
          }
          break;
        }

        text += decoder.decode(value, { stream: true });
        capturedBytes += value.byteLength;
      }

      if (!truncated) text += decoder.decode();
    } catch (error) {
      appendEvent('protocol-read-response-error', {
        ...metadata,
        capturedBytes,
        message: String(error?.message ?? error),
      });
      return;
    }

    readCaptureBytes += capturedBytes;
    readCaptureResponses += 1;
    appendEvent('protocol-read-response-captured', {
      ...metadata,
      capturedBytes,
      truncated,
      bodyPresent: true,
      bodyText: text,
    });
    scheduleStatusRefresh();

    if (readCaptureBytes >= MAX_READ_RUN_BYTES) {
      appendEvent('protocol-read-capture-limit', {
        limitBytes: MAX_READ_RUN_BYTES,
        capturedBytes: readCaptureBytes,
      });
      disarmReadCapture('run-byte-limit');
    }
  }

  async function captureNetworkResponseStream(response, requestInfo) {
    const streamId = makeId('network-stream');
    const contentType = response.headers.get('content-type') ?? '';
    const endpoint = requestInfo.url?.pathname ?? '/backend-api/f/conversation';
    let totalBytes = 0;
    let chunkIndex = 0;
    let truncated = false;
    const protocolState = createProtocolStreamState(streamId);

    appendEvent('network-stream-start', {
      streamId,
      endpoint,
      method: requestInfo.method,
      status: response.status,
      contentType,
      privateEvidence: true,
    });

    try {
      if (!response.body) {
        appendEvent('network-stream-unavailable', {
          streamId,
          endpoint,
          reason: 'response-body-unavailable',
        });
        return;
      }

      const reader = response.body.getReader();
      const decoder = new TextDecoder();
      let terminalProbe = '';
      let sawSseDone = false;

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        if (!value?.byteLength) continue;

        if (totalBytes + value.byteLength > MAX_NETWORK_STREAM_BYTES) {
          truncated = true;
          appendEvent('network-stream-truncated', {
            streamId,
            endpoint,
            capturedBytes: totalBytes,
            limitBytes: MAX_NETWORK_STREAM_BYTES,
          });
          try {
            await reader.cancel('Chatarium network stream capture limit reached');
          } catch {
            // The original response branch belongs to ChatGPT. Failure to cancel our clone is
            // diagnostic-only and must never affect the page's response consumption.
          }
          break;
        }

        totalBytes += value.byteLength;
        const text = decoder.decode(value, { stream: true });
        appendEvent('network-stream-chunk', {
          streamId,
          endpoint,
          chunkIndex,
          bytes: value.byteLength,
          text,
          privateEvidence: true,
        });
        feedProtocolSseText(protocolState, text);
        chunkIndex += 1;

        terminalProbe = (terminalProbe + text).slice(-256);
        if (/(?:^|\r?\n)data:\s*\[DONE\](?:\r?\n|$)/.test(terminalProbe)) {
          sawSseDone = true;
          break;
        }
      }

      if (!truncated) {
        const tail = decoder.decode();
        if (tail) {
          appendEvent('network-stream-chunk', {
            streamId,
            endpoint,
            chunkIndex,
            bytes: 0,
            text: tail,
            privateEvidence: true,
          });
          feedProtocolSseText(protocolState, tail);
          chunkIndex += 1;
        }
        if (sawSseDone) {
          try {
            await reader.cancel('Chatarium observed terminal SSE [DONE]');
          } catch {
            // The terminal marker is already durable evidence. A clone-cancel failure after
            // [DONE] does not change the observed remote outcome.
          }
        }
        appendEvent('network-stream-end', {
          streamId,
          endpoint,
          chunks: chunkIndex,
          capturedBytes: totalBytes,
          terminal: sawSseDone ? 'sse-done' : 'eof',
          privateEvidence: true,
        });
      }
    } catch (error) {
      appendEvent('network-stream-error', {
        streamId,
        endpoint,
        capturedBytes: totalBytes,
        message: String(error?.message ?? error),
      });
    }
  }

  function installNetworkStreamCapture() {
    if (typeof window.fetch !== 'function') return;

    const originalFetch = window.fetch;
    window.fetch = new Proxy(originalFetch, {
      apply(target, thisArg, args) {
        const requestInfo = networkRequestInfo(args[0], args[1]);
        const captureRead = readCaptureArmed && requestInfo.readEligible;
        const result = Reflect.apply(target, thisArg, args);
        if (!requestInfo.captureStream && !captureRead) return result;

        return Promise.resolve(result).then(
          (response) => {
            if (requestInfo.captureStream) {
              try {
                const clone = response.clone();
                void captureNetworkResponseStream(clone, requestInfo);
              } catch (error) {
                appendEvent('network-stream-error', {
                  endpoint: requestInfo.url?.pathname ?? '/backend-api/f/conversation',
                  capturedBytes: 0,
                  message: `clone response: ${String(error?.message ?? error)}`,
                });
              }
            }

            if (captureRead) {
              try {
                const clone = response.clone();
                void captureProtocolReadResponse(clone, requestInfo);
              } catch (error) {
                appendEvent('protocol-read-response-error', {
                  method: requestInfo.method,
                  endpoint: requestInfo.url?.pathname ?? '<unknown>',
                  queryKeys: requestInfo.queryKeys,
                  capturedBytes: 0,
                  message: `clone response: ${String(error?.message ?? error)}`,
                });
              }
            }

            return response;
          },
          (error) => {
            if (requestInfo.captureStream) {
              appendEvent('network-fetch-error', {
                endpoint: requestInfo.url?.pathname ?? '/backend-api/f/conversation',
                method: requestInfo.method,
                message: String(error?.message ?? error),
              });
            }
            if (captureRead) {
              appendEvent('protocol-read-fetch-error', {
                method: requestInfo.method,
                endpoint: requestInfo.url?.pathname ?? '<unknown>',
                queryKeys: requestInfo.queryKeys,
                message: String(error?.message ?? error),
              });
            }
            throw error;
          },
        );
      },
    });
  }

  function composer() {
    return document.querySelector('#prompt-textarea') ??
      document.querySelector('textarea[placeholder]') ??
      document.querySelector('[contenteditable="true"][data-virtualkeyboard]') ??
      document.querySelector('main [contenteditable="true"]');
  }

  function composerFromEventTarget(target) {
    if (!(target instanceof Element)) return composer();
    const candidate = target.closest(
      '#prompt-textarea, textarea[placeholder], [contenteditable="true"][data-virtualkeyboard], main [contenteditable="true"]',
    );
    return candidate ?? composer();
  }

  function composerText(node = composer()) {
    if (!node) return '';
    if ('value' in node && typeof node.value === 'string') return node.value;
    return node.innerText ?? node.textContent ?? '';
  }

  function draftWalKey(conversation = conversationKey()) {
    return `${DRAFT_WAL_PREFIX}${conversation}`;
  }

  function readDraftWal(conversation = conversationKey()) {
    return safeJsonParse(localStorage.getItem(draftWalKey(conversation)), null);
  }

  function writeDraftWal(kind, text) {
    const record = {
      version: 3,
      at: now(),
      href: location.href,
      conversation: conversationKey(),
      kind,
      text,
    };
    writeLocalJson(draftWalKey(record.conversation), record, 'draft WAL');
    return record;
  }

  function readSendIntents() {
    const value = safeJsonParse(localStorage.getItem(SEND_WAL_KEY), []);
    return Array.isArray(value) ? value : [];
  }

  function writeSendIntents(intents) {
    const trimmed = intents.slice(-MAX_SEND_INTENTS);
    writeLocalJson(SEND_WAL_KEY, trimmed, 'send WAL');
    scheduleStatusRefresh();
    return trimmed;
  }

  function readAssistantWal() {
    return safeJsonParse(localStorage.getItem(ASSISTANT_WAL_KEY), null);
  }

  function writeAssistantWal(record) {
    const text = String(record.text ?? '');
    const truncated = text.length > MAX_ASSISTANT_WAL_CHARS;
    const storedText = truncated ? text.slice(-MAX_ASSISTANT_WAL_CHARS) : text;
    const previous = readAssistantWal();
    const incomingSource = record.source ?? 'dom-transcript';
    const sameObservedMessage = Boolean(
      previous &&
      previous.observedId &&
      record.observedId &&
      previous.observedId === record.observedId
    );
    const previousSources = sameObservedMessage
      ? (Array.isArray(previous.evidenceSources)
        ? previous.evidenceSources
        : [previous.source].filter(Boolean))
      : [];
    const evidenceSources = [...new Set([...previousSources, incomingSource])];
    const wal = {
      version: 3,
      at: now(),
      href: record.href ?? location.href,
      conversation: record.conversation ?? conversationKey(),
      observedId: record.observedId ?? null,
      text: storedText,
      originalChars: text.length,
      truncatedPrefix: truncated,
      source: incomingSource,
      evidenceSources,
      protocolEvidence: record.protocolEvidence ?? (sameObservedMessage ? previous.protocolEvidence : null) ?? null,
      protocolStatus: record.protocolStatus ?? (sameObservedMessage ? previous.protocolStatus : null) ?? null,
      protocolEndTurn: record.protocolEndTurn ?? (sameObservedMessage ? previous.protocolEndTurn : null) ?? null,
      protocolIsComplete: record.protocolIsComplete ?? (sameObservedMessage ? previous.protocolIsComplete : null) ?? null,
    };
    writeLocalJson(ASSISTANT_WAL_KEY, wal, 'assistant WAL');
    return wal;
  }

  function readErrorWal() {
    return safeJsonParse(localStorage.getItem(ERROR_WAL_KEY), null);
  }

  function writeErrorWal(text) {
    const normalized = normalizeText(text);
    if (!normalized) return null;
    const wal = {
      version: 3,
      at: now(),
      href: location.href,
      conversation: conversationKey(),
      text: normalized.slice(0, MAX_ERROR_WAL_CHARS),
      truncated: normalized.length > MAX_ERROR_WAL_CHARS,
    };
    writeLocalJson(ERROR_WAL_KEY, wal, 'visible error WAL');
    return wal;
  }

  function migrateLegacyWal() {
    const legacy = safeJsonParse(localStorage.getItem(LEGACY_WAL_KEY), null);
    if (!legacy) return;

    if (legacy.kind === 'send-intent' && normalizeText(legacy.text)) {
      const existing = readSendIntents();
      const duplicate = existing.some((intent) => intent.at === legacy.at && intent.text === legacy.text);
      if (!duplicate) {
        existing.push({
          id: makeId('legacy-send'),
          version: 3,
          at: legacy.at ?? now(),
          href: legacy.href ?? location.href,
          conversation: legacy.conversation ?? conversationKey(),
          reason: 'legacy-wal-migration',
          text: legacy.text,
          state: 'unknown',
          confirmedAt: null,
          observedMessageId: null,
        });
        writeSendIntents(existing);
      }
    } else if (normalizeText(legacy.text) && !readDraftWal()) {
      const migrated = { ...legacy, version: 3 };
      writeLocalJson(
        draftWalKey(migrated.conversation ?? conversationKey()),
        migrated,
        'legacy draft migration',
      );
    }

    try {
      localStorage.removeItem(LEGACY_WAL_KEY);
    } catch {
      // Best effort migration cleanup only.
    }
  }

  function snapshotDraftFromNode(node, reason = 'mutation') {
    if (!node) return null;
    const text = composerText(node);
    const record = writeDraftWal('draft', text);
    const draft = { ...record, reason };
    lastPolledComposerFingerprint = `${record.conversation}\u0000${text}`;
    void tx('drafts', 'readwrite', (store) => store.put(draft)).catch((error) => {
      console.error('[chatarium] draft archive write failed', error);
    });
    scheduleStatusRefresh();
    return draft;
  }

  function snapshotDraft(reason = 'mutation') {
    return snapshotDraftFromNode(composer(), reason);
  }

  function snapshotSendIntent(reason) {
    const text = normalizeText(composerText());
    if (!text) return null;

    const intent = {
      id: makeId('send'),
      version: 3,
      at: now(),
      href: location.href,
      conversation: conversationKey(),
      reason,
      text,
      state: 'pending',
      confirmedAt: null,
      observedMessageId: null,
    };

    const intents = readSendIntents();
    const previous = intents.at(-1);
    const likelyDuplicate = previous &&
      previous.state === 'pending' &&
      previous.conversation === intent.conversation &&
      previous.text === intent.text &&
      Date.now() - Date.parse(previous.at) < 1500;

    if (!likelyDuplicate) {
      intents.push(intent);
      writeSendIntents(intents);
      appendEvent('send-intent', {
        id: intent.id,
        reason,
        text,
      });
    }

    return likelyDuplicate ? previous : intent;
  }

  function scheduleDraft(reason, node = null) {
    clearTimeout(draftTimer);
    draftTimer = setTimeout(() => {
      if (node?.isConnected) snapshotDraftFromNode(node, reason);
      else snapshotDraft(reason);
    }, 120);
  }

  function monitorComposer() {
    setInterval(() => {
      const node = composer();
      if (!node) return;
      const text = composerText(node);
      const fingerprint = `${conversationKey()}\u0000${text}`;
      if (fingerprint === lastPolledComposerFingerprint) return;
      snapshotDraftFromNode(node, 'poll');
    }, COMPOSER_POLL_MS);
  }

  function messageText(node) {
    return normalizeText(node.innerText ?? node.textContent ?? '');
  }

  function fallbackHash(text) {
    let hash = 2166136261;
    for (let i = 0; i < text.length; i += 1) {
      hash ^= text.charCodeAt(i);
      hash = Math.imul(hash, 16777619);
    }
    return (hash >>> 0).toString(16).padStart(8, '0');
  }

  function clearDraftIfMatches(conversation, text) {
    const draft = readDraftWal(conversation);
    if (!draft || normalizeText(draft.text) !== normalizeText(text)) return;
    const cleared = {
      version: 3,
      at: now(),
      href: location.href,
      conversation,
      kind: 'confirmed-send-cleared',
      text: '',
    };
    writeLocalJson(draftWalKey(conversation), cleared, 'confirmed draft clear');
  }

  function confirmObservedUserMessage(text, observedMessageId, options = {}) {
    const normalized = normalizeText(text);
    if (!normalized) return;

    const confirmedConversation = options.conversation ?? conversationKey();
    const evidence = options.evidence ?? 'dom-transcript';
    const intents = readSendIntents();
    let changed = false;

    for (let index = intents.length - 1; index >= 0; index -= 1) {
      const intent = intents[index];
      if (intent.state !== 'pending') continue;
      if (normalizeText(intent.text) !== normalized) continue;

      const ageMs = Date.now() - Date.parse(intent.at);
      const sameConversation = intent.conversation === confirmedConversation;
      const recentPreConversationRoute = intent.conversation.startsWith('route:') && ageMs >= 0 && ageMs < 120_000;
      if (!sameConversation && !recentPreConversationRoute) continue;

      const originConversation = intent.conversation;
      intent.state = 'confirmed';
      intent.confirmedAt = now();
      intent.confirmedConversation = confirmedConversation;
      intent.observedMessageId = observedMessageId ?? null;
      intent.confirmationEvidence = evidence;
      clearDraftIfMatches(originConversation, normalized);
      if (originConversation !== confirmedConversation) clearDraftIfMatches(confirmedConversation, normalized);
      changed = true;
      appendEvent('send-confirmed', {
        id: intent.id,
        observedMessageId: observedMessageId ?? null,
        confirmedConversation,
        evidence,
      });
      break;
    }

    if (changed) writeSendIntents(intents);
  }

  function captureVisibleErrors() {
    const candidates = [
      ...document.querySelectorAll('[role="alert"]'),
      ...document.querySelectorAll('[data-sonner-toast]'),
    ];

    for (const node of candidates) {
      const text = messageText(node);
      if (!text || observedErrors.has(text)) continue;
      observedErrors.add(text);
      writeErrorWal(text);
      appendEvent('visible-error-observed', { text: text.slice(0, MAX_ERROR_WAL_CHARS) });
    }
  }

  function captureTranscript() {
    const nodes = [...document.querySelectorAll('[data-message-author-role]')];
    let latestAssistant = null;

    nodes.forEach((node, index) => {
      const role = node.getAttribute('data-message-author-role') ?? 'unknown';
      const text = messageText(node);
      if (!text) return;

      const envelope = node.closest('[data-message-id]');
      const observedId = envelope?.getAttribute('data-message-id') ?? node.getAttribute('data-message-id');
      const transientStatus = role === 'assistant' && String(observedId ?? '').startsWith('request-placeholder-');
      const key = observedId
        ? `${conversationKey()}:id:${observedId}`
        : `${conversationKey()}:${role}:${index}`;
      const record = {
        key,
        conversation: conversationKey(),
        href: location.href,
        observedAt: now(),
        observedId: observedId ?? null,
        role,
        index,
        contentHash: fallbackHash(text),
        transientStatus,
        text,
      };

      void tx('messages', 'readwrite', (store) => store.put(record)).catch((error) => {
        console.error('[chatarium] transcript write failed', error);
      });

      if (role === 'user') confirmObservedUserMessage(text, observedId ?? null);
      if (role === 'assistant' && !transientStatus) latestAssistant = record;
    });

    if (latestAssistant) writeAssistantWal(latestAssistant);
    captureVisibleErrors();
    scheduleStatusRefresh();
  }

  function scheduleTranscript() {
    clearTimeout(transcriptTimer);
    transcriptTimer = setTimeout(captureTranscript, 120);
  }

  function looksLikeSendButton(target) {
    const button = target instanceof Element ? target.closest('button') : null;
    if (!button) return false;
    const testId = button.getAttribute('data-testid') ?? '';
    const aria = button.getAttribute('aria-label') ?? '';
    return /send/i.test(testId) || /send/i.test(aria);
  }

  function installEventCapture() {
    document.addEventListener('input', (event) => {
      const activeComposer = composer();
      const eventComposer = composerFromEventTarget(event.target);
      if (
        eventComposer &&
        activeComposer &&
        (eventComposer === activeComposer || activeComposer.contains?.(event.target))
      ) {
        scheduleDraft('input', eventComposer);
      }
    }, true);

    document.addEventListener('pointerdown', (event) => {
      if (looksLikeSendButton(event.target)) {
        snapshotDraft('before-send-button');
        snapshotSendIntent('send-button-pointerdown');
      }
    }, true);

    document.addEventListener('submit', (event) => {
      const activeComposer = composer();
      const form = event.target instanceof HTMLFormElement ? event.target : null;
      if (activeComposer && form?.contains(activeComposer)) {
        snapshotDraft('before-form-submit');
        snapshotSendIntent('form-submit');
      }
    }, true);

    document.addEventListener('keydown', (event) => {
      if (
        event.ctrlKey === EXPORT_HOTKEY.ctrlKey &&
        event.shiftKey === EXPORT_HOTKEY.shiftKey &&
        event.altKey === EXPORT_HOTKEY.altKey &&
        event.code === EXPORT_HOTKEY.code
      ) {
        event.preventDefault();
        void exportAll();
        return;
      }

      const activeComposer = composer();
      if (!activeComposer || !(event.target === activeComposer || activeComposer.contains?.(event.target))) return;
      if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
        snapshotDraftFromNode(activeComposer, 'before-composer-enter');
        snapshotSendIntent('composer-enter');
      }
    }, true);
  }

  function installMutationCapture() {
    const observer = new MutationObserver(() => {
      scheduleTranscript();
    });
    observer.observe(document.documentElement, { childList: true, subtree: true, characterData: true });
  }

  function monitorNavigation() {
    setInterval(() => {
      if (location.href === lastHref) return;
      const previous = lastHref;
      lastHref = location.href;
      appendEvent('navigation', { from: previous, to: lastHref });
      lastPolledComposerFingerprint = null;
      scheduleDraft('navigation');
      scheduleTranscript();
      scheduleStatusRefresh();
    }, 500);
  }

  function monitorConnectivity() {
    addEventListener('offline', () => {
      snapshotDraft('browser-offline');
      appendEvent('browser-offline');
      scheduleStatusRefresh();
    });
    addEventListener('online', () => {
      appendEvent('browser-online');
      scheduleStatusRefresh();
    });
    addEventListener('beforeunload', () => {
      snapshotDraft('beforeunload');
      captureTranscript();
    }, true);
  }

  async function readStore(name) {
    const db = await openDb();
    return new Promise((resolve, reject) => {
      const transaction = db.transaction(name, 'readonly');
      const request = transaction.objectStore(name).getAll();
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
  }

  async function exportAll() {
    // The synchronous localStorage WAL is refreshed immediately before export. This deliberately
    // does not trust that React/DOM input events fired correctly.
    snapshotDraft('export');
    captureTranscript();
    const [events, drafts, messages] = await Promise.all([
      readStore('events'),
      readStore('drafts'),
      readStore('messages'),
    ]);
    const payload = {
      format: 'chatarium-flight-recorder-export',
      version: 3,
      recorderVersion: VERSION,
      exportedAt: now(),
      href: location.href,
      draftWal: readDraftWal(),
      sendIntents: readSendIntents(),
      assistantWal: readAssistantWal(),
      lastVisibleError: readErrorWal(),
      events,
      drafts,
      messages,
      protocolReadCapture: readCaptureStatus(),
    };

    const blob = new Blob([JSON.stringify(payload, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = `chatarium-flight-recorder-${Date.now()}.json`;
    document.documentElement.appendChild(anchor);
    anchor.click();
    anchor.remove();
    URL.revokeObjectURL(url);
    appendEvent('local-export-created', { eventCount: events.length, messageCount: messages.length });
    console.info('[chatarium] export complete', payload);
    return payload;
  }

  async function copyText(text) {
    if (!text) return false;
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch (error) {
      console.error('[chatarium] clipboard write failed', error);
      return false;
    }
  }

  async function copySavedDraft() {
    const draft = readDraftWal();
    return copyText(draft?.text ?? '');
  }

  async function copyLatestSendIntent() {
    const latest = readSendIntents().at(-1);
    return copyText(latest?.text ?? '');
  }

  async function copyLatestAssistant() {
    const latest = readAssistantWal();
    return copyText(latest?.text ?? '');
  }

  async function status() {
    const intents = readSendIntents();
    return {
      recorderVersion: VERSION,
      href: location.href,
      conversation: conversationKey(),
      online: navigator.onLine,
      draftWal: readDraftWal(),
      sendIntents: intents,
      pendingSendIntents: intents.filter((intent) => intent.state !== 'confirmed'),
      assistantWal: readAssistantWal(),
      lastVisibleError: readErrorWal(),
      protocolReadCapture: readCaptureStatus(),
      counts: {
        events: (await readStore('events')).length,
        drafts: (await readStore('drafts')).length,
        messages: (await readStore('messages')).length,
      },
    };
  }

  function ensurePanel() {
    if (panelHost?.isConnected) return;
    if (!document.body) return;

    panelHost = document.createElement('div');
    panelHost.id = 'chatarium-flight-recorder-host';
    panelHost.style.position = 'fixed';
    panelHost.style.right = '12px';
    panelHost.style.bottom = '12px';
    panelHost.style.zIndex = '2147483647';
    panelHost.style.font = '12px/1.35 system-ui, sans-serif';
    panelRoot = panelHost.attachShadow({ mode: 'open' });
    document.body.appendChild(panelHost);
    renderPanel();
  }

  function renderPanel() {
    ensurePanel();
    if (!panelRoot) return;

    const intents = readSendIntents();
    const pending = intents.filter((intent) => intent.state !== 'confirmed');
    const draft = readDraftWal();
    const assistant = readAssistantWal();
    const error = readErrorWal();
    const hasDraft = Boolean(normalizeText(draft?.text));
    const hasAssistant = Boolean(normalizeText(assistant?.text));
    const latestPending = pending.at(-1);
    const assistantChars = assistant?.originalChars ?? assistant?.text?.length ?? 0;
    const readState = readCaptureStatus();

    panelRoot.innerHTML = `
      <style>
        :host { all: initial; }
        .box {
          color: #f4f4f5;
          background: rgba(24, 24, 27, 0.96);
          border: 1px solid rgba(255,255,255,0.18);
          border-radius: 10px;
          box-shadow: 0 8px 30px rgba(0,0,0,0.35);
          padding: 8px;
          min-width: 220px;
          max-width: 320px;
          font: 12px/1.35 system-ui, sans-serif;
        }
        .row { display: flex; align-items: center; gap: 6px; justify-content: space-between; }
        .state { font-weight: 700; }
        .sub { opacity: 0.72; margin-top: 4px; overflow-wrap: anywhere; }
        .actions { display: flex; flex-wrap: wrap; gap: 5px; margin-top: 7px; }
        button {
          all: unset;
          cursor: pointer;
          background: rgba(255,255,255,0.10);
          padding: 4px 7px;
          border-radius: 6px;
        }
        button:hover { background: rgba(255,255,255,0.18); }
        button[disabled] { cursor: default; opacity: 0.35; }
        .ok { color: #86efac; }
        .warn { color: #fde68a; }
        .bad { color: #fca5a5; }
        .armed { color: #fca5a5; font-weight: 700; }
      </style>
      <div class="box">
        <div class="row">
          <span class="state ${navigator.onLine ? 'ok' : 'bad'}">Chatarium ${navigator.onLine ? '●' : '○'}</span>
          <span>v${VERSION}</span>
        </div>
        <div class="sub">${hasDraft ? 'draft saved' : 'no non-empty draft'} · ${pending.length} unresolved send${pending.length === 1 ? '' : 's'}</div>
        <div class="sub">${hasAssistant ? `assistant ${assistantChars.toLocaleString()} chars saved` : 'no assistant snapshot yet'}</div>
        <div class="sub ${readState.armed ? 'armed' : ''}">protocol reads: ${readState.armed ? `ARMED · ${readState.responseCount} response(s) · ${readState.capturedBytes.toLocaleString()} bytes` : 'off'}</div>
        ${latestPending ? `<div class="sub warn">latest unresolved: ${escapeHtml(latestPending.at)}</div>` : ''}
        ${error ? `<div class="sub bad">last site error: ${escapeHtml(error.text.slice(0, 180))}</div>` : ''}
        <div class="actions">
          <button data-action="copy-draft" ${hasDraft ? '' : 'disabled'}>Copy draft</button>
          <button data-action="copy-send" ${intents.length ? '' : 'disabled'}>Copy last send</button>
          <button data-action="copy-assistant" ${hasAssistant ? '' : 'disabled'}>Copy assistant</button>
          <button data-action="toggle-read">${readState.armed ? 'Disarm reads' : 'Arm reads'}</button>
          <button data-action="export">Export</button>
        </div>
      </div>
    `;

    panelRoot.querySelector('[data-action="copy-draft"]')?.addEventListener('click', () => void copySavedDraft());
    panelRoot.querySelector('[data-action="copy-send"]')?.addEventListener('click', () => void copyLatestSendIntent());
    panelRoot.querySelector('[data-action="copy-assistant"]')?.addEventListener('click', () => void copyLatestAssistant());
    panelRoot.querySelector('[data-action="toggle-read"]')?.addEventListener('click', () => {
      if (readCaptureArmed) disarmReadCapture('operator');
      else armReadCapture();
    });
    panelRoot.querySelector('[data-action="export"]')?.addEventListener('click', () => void exportAll());
  }

  function escapeHtml(value) {
    return String(value)
      .replaceAll('&', '&amp;')
      .replaceAll('<', '&lt;')
      .replaceAll('>', '&gt;')
      .replaceAll('"', '&quot;')
      .replaceAll("'", '&#39;');
  }

  function scheduleStatusRefresh() {
    clearTimeout(statusRefreshTimer);
    statusRefreshTimer = setTimeout(renderPanel, 120);
  }

  Object.defineProperty(window, 'ChatariumFlightRecorder', {
    configurable: false,
    enumerable: false,
    writable: false,
    value: Object.freeze({
      version: VERSION,
      exportAll,
      status,
      captureTranscript,
      snapshotDraft,
      snapshotSendIntent,
      copySavedDraft,
      copyLatestSendIntent,
      copyLatestAssistant,
      readSendIntents,
      readAssistantWal,
      readErrorWal,
      armReadCapture,
      disarmReadCapture,
      readCaptureStatus,
    }),
  });

  installNetworkStreamCapture();
  migrateLegacyWal();
  installEventCapture();
  installMutationCapture();
  monitorNavigation();
  monitorConnectivity();
  monitorComposer();
  void openDb().then(() => appendEvent('recorder-started', { version: VERSION })).catch(console.error);

  addEventListener('DOMContentLoaded', () => {
    snapshotDraft('dom-content-loaded');
    scheduleTranscript();
    ensurePanel();
  }, { once: true });

  if (document.readyState !== 'loading') {
    snapshotDraft('late-install');
    ensurePanel();
  }
  console.info('[chatarium] flight recorder active; Ctrl+Shift+Alt+E exports local evidence');
})();
