# Chatarium Edge Bridge

This directory contains the **critical-path browser runtime** for Chatarium's read-only access to the user's existing ChatGPT conversation corpus.

It replaces the retired Tampermonkey account-history prototype. The flight recorder elsewhere in `browser/` remains a separate durability/evidence tool; it is not this transport.

## Architecture

```text
Chatarium Desktop
    ↕ typed HTTP loopback on 127.0.0.1:43117
MV3 extension service worker
    ↕ chrome.scripting.executeScript({ world: "MAIN" })
authenticated chatgpt.com tab
    ↕ same-origin GET
/backend-api/*
```

The extension performs three deliberately separate jobs:

1. poll Chatarium's typed loopback command queue and return typed results;
2. observe the already-present `ChatGPT-Account-ID` header on first-party ChatGPT requests using `chrome.webRequest`;
3. execute the evidence-backed authenticated GET inside the exact ChatGPT tab's MAIN world.

The raw account identifier stays in `chrome.storage.session` under a per-tab key. It is never returned to Rust, printed in diagnostics, or written to Chatarium's journal.

## Permissions

The manifest is intentionally narrow:

- `scripting`
- `storage`
- `webRequest`
- host access to `https://chatgpt.com/*`
- host access to `http://127.0.0.1:43117/*`

Do not add broad browsing permissions, cookies access, debugger access, or `<all_urls>` merely to make integration easier. A broader permission request requires explicit evidence and documentation.

## Supported commands

The loopback wire protocol remains version 1 and supports only:

- `probe_auth`
- `list_conversations`
- `fetch_conversation`

The desktop supplies the exact evidence-backed resource and request-profile revision. The extension rejects profile/resource mismatches rather than inventing a nearby request.

Current profiles:

- authentication probe: `chatgpt-me-v1`
- first account-history page: `2026-10-03.002`
- exact C02 conversation read: `2026-10-03.001`

## Proof ladder

A successful result carries only safe proof metadata:

- extension version;
- desktop roundtrip (established when Rust receives the correlated result);
- exact ChatGPT tab found;
- MAIN-world execution;
- account-context presence;
- request-profile identity;
- HTTP status.

Rust then adds parser/semantic proof. Exact-conversation synchronization does not become a complete success until the matching validated mirror is durably committed to the local journal.

A lower-level success must never be presented as a higher-level success.

## Account-context observation

The extension does not guess, hard-code, or ask Rust for the active ChatGPT account identifier.

It passively observes `ChatGPT-Account-ID` on ChatGPT's own first-party `/backend-api/*` requests, validates a narrow identifier grammar, and stores the value only for that tab in extension session storage. Closing the tab or navigating it away from `chatgpt.com` removes the cached context.

If no valid account context has been observed, account-history reads fail closed with `account_context_unavailable`.

## Loopback

The service worker talks only to `127.0.0.1:43117` and sends the generation marker `X-Chatarium-Bridge: edge-mv3-v1`. Chatarium independently enforces loopback binding, rejects ordinary web origins/preflight, accepts only the Chromium extension-origin shape for browser-originated traffic, enforces request-size limits and command/result correlation, and rejects the retired userscript marker `1`. This means an old installed `account-bridge.user.js` cannot consume the extension command queue.

The extension does not use Native Messaging. Native Messaging remains a fallback architecture only if the instrumented MV3 extension cannot reliably reach loopback on the target browser.

## Sideloading for the eventual live QA

Live operator QA is intentionally blocked until repository checks are green.

When that gate is satisfied, Edge can load this directory as an unpacked extension through the browser's extension-management page. After loading or reloading the extension, reload/open an authenticated `https://chatgpt.com/` tab so first-party traffic can supply account context. Chatarium Desktop then performs the proof-ladder check itself; DevTools should not be required.

Do not install or test this extension merely because files exist here. Follow `docs/HUMAN_QA.md` and the current #102 handoff.

## Automated package check

Run:

```sh
node --check browser/edge-bridge/service-worker.js
node browser/edge-bridge/check.mjs
```

The package check freezes the narrow manifest permissions and rejects accidental Tampermonkey/GM dependencies in the replacement runtime.
