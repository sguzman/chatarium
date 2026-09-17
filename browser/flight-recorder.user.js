// ==UserScript==
// @name         Chatarium Flight Recorder
// @namespace    https://github.com/sguzman/chatarium
// @version      0.1.0
// @description  Local durability layer for ChatGPT drafts and observed transcript text.
// @match        https://chatgpt.com/*
// @run-at       document-start
// @grant        none
// ==/UserScript==

(() => {
  'use strict';

  const DB_NAME = 'chatarium-flight-recorder';
  const DB_VERSION = 1;
  const WAL_KEY = 'chatarium:p0:wal';
  const EXPORT_HOTKEY = { ctrlKey: true, shiftKey: true, altKey: true, code: 'KeyE' };

  let dbPromise;
  let lastHref = location.href;
  let transcriptTimer = 0;
  let draftTimer = 0;

  const now = () => new Date().toISOString();

  function conversationKey() {
    const match = location.pathname.match(/^\/c\/([^/?#]+)/);
    return match ? `conversation:${match[1]}` : `route:${location.pathname}${location.search}`;
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
    void tx('events', 'readwrite', (store) => store.add(event)).catch(console.error);
  }

  function composer() {
    return document.querySelector('#prompt-textarea') ??
      document.querySelector('textarea[placeholder]') ??
      document.querySelector('[contenteditable="true"][data-virtualkeyboard]') ??
      document.querySelector('main [contenteditable="true"]');
  }

  function composerText(node = composer()) {
    if (!node) return '';
    if ('value' in node && typeof node.value === 'string') return node.value;
    return node.innerText ?? node.textContent ?? '';
  }

  function writeWal(kind, text) {
    // Synchronous by design: this is the last-resort write-ahead record that should
    // complete before the site's own click/submit handler gets a chance to destroy UI state.
    const record = {
      version: 1,
      at: now(),
      href: location.href,
      conversation: conversationKey(),
      kind,
      text,
    };
    try {
      localStorage.setItem(WAL_KEY, JSON.stringify(record));
    } catch (error) {
      console.error('[chatarium] WAL write failed', error);
    }
    return record;
  }

  function snapshotDraft(reason = 'mutation') {
    const text = composerText();
    const record = writeWal('draft', text);
    const draft = { ...record, reason };
    void tx('drafts', 'readwrite', (store) => store.put(draft)).catch(console.error);
  }

  function snapshotSendIntent(reason) {
    const text = composerText();
    const record = writeWal('send-intent', text);
    appendEvent('send-intent', { reason, text, walAt: record.at });
  }

  function scheduleDraft(reason) {
    clearTimeout(draftTimer);
    draftTimer = setTimeout(() => snapshotDraft(reason), 120);
  }

  function messageText(node) {
    return (node.innerText ?? node.textContent ?? '').trim();
  }

  function fallbackHash(text) {
    let hash = 2166136261;
    for (let i = 0; i < text.length; i += 1) {
      hash ^= text.charCodeAt(i);
      hash = Math.imul(hash, 16777619);
    }
    return (hash >>> 0).toString(16).padStart(8, '0');
  }

  function captureTranscript() {
    const nodes = [...document.querySelectorAll('[data-message-author-role]')];
    nodes.forEach((node, index) => {
      const role = node.getAttribute('data-message-author-role') ?? 'unknown';
      const text = messageText(node);
      if (!text) return;

      const envelope = node.closest('[data-message-id]');
      const observedId = envelope?.getAttribute('data-message-id') ?? node.getAttribute('data-message-id');
      const key = observedId
        ? `${conversationKey()}:id:${observedId}`
        : `${conversationKey()}:${role}:${index}:${fallbackHash(text)}`;

      const record = {
        key,
        conversation: conversationKey(),
        href: location.href,
        observedAt: now(),
        observedId: observedId ?? null,
        role,
        index,
        text,
      };
      void tx('messages', 'readwrite', (store) => store.put(record)).catch(console.error);
    });
  }

  function scheduleTranscript() {
    clearTimeout(transcriptTimer);
    transcriptTimer = setTimeout(captureTranscript, 180);
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
      if (event.target === composer() || composer()?.contains?.(event.target)) {
        scheduleDraft('input');
      }
    }, true);

    document.addEventListener('pointerdown', (event) => {
      if (looksLikeSendButton(event.target)) snapshotSendIntent('send-button-pointerdown');
    }, true);

    document.addEventListener('submit', () => snapshotSendIntent('form-submit'), true);

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
      if (event.key === 'Enter' && !event.shiftKey) snapshotSendIntent('composer-enter');
    }, true);
  }

  function installMutationCapture() {
    const observer = new MutationObserver(() => {
      scheduleTranscript();
      scheduleDraft('dom-mutation');
    });
    observer.observe(document.documentElement, { childList: true, subtree: true, characterData: true });
  }

  function monitorNavigation() {
    setInterval(() => {
      if (location.href === lastHref) return;
      const previous = lastHref;
      lastHref = location.href;
      appendEvent('navigation', { from: previous, to: lastHref });
      snapshotDraft('navigation');
      scheduleTranscript();
    }, 500);
  }

  function monitorConnectivity() {
    addEventListener('offline', () => appendEvent('browser-offline'));
    addEventListener('online', () => appendEvent('browser-online'));
    addEventListener('beforeunload', () => {
      snapshotDraft('beforeunload');
      writeWal('beforeunload-draft', composerText());
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
    const [events, drafts, messages] = await Promise.all([
      readStore('events'),
      readStore('drafts'),
      readStore('messages'),
    ]);
    const wal = localStorage.getItem(WAL_KEY);
    const payload = {
      format: 'chatarium-flight-recorder-export',
      version: 1,
      exportedAt: now(),
      href: location.href,
      wal: wal ? JSON.parse(wal) : null,
      events,
      drafts,
      messages,
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
    console.info('[chatarium] export complete', payload);
    return payload;
  }

  async function status() {
    return {
      href: location.href,
      conversation: conversationKey(),
      wal: JSON.parse(localStorage.getItem(WAL_KEY) ?? 'null'),
      counts: {
        events: (await readStore('events')).length,
        drafts: (await readStore('drafts')).length,
        messages: (await readStore('messages')).length,
      },
    };
  }

  Object.defineProperty(window, 'ChatariumFlightRecorder', {
    configurable: false,
    enumerable: false,
    writable: false,
    value: Object.freeze({ exportAll, status, captureTranscript, snapshotDraft }),
  });

  installEventCapture();
  installMutationCapture();
  monitorNavigation();
  monitorConnectivity();
  void openDb().then(() => appendEvent('recorder-started', { version: '0.1.0' })).catch(console.error);

  addEventListener('DOMContentLoaded', () => {
    snapshotDraft('dom-content-loaded');
    scheduleTranscript();
  }, { once: true });

  console.info('[chatarium] flight recorder active; Ctrl+Shift+Alt+E exports local evidence');
})();
