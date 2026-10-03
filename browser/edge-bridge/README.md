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
2. observe the exact first-party ordinary-history request context using `chrome.webRequest`;
3. execute the evidence-backed authenticated GET inside the exact ChatGPT tab's MAIN world using that observed application context.

For the ordinary-history request, the extension retains only a narrow allowlist of application-controlled headers already emitted by ChatGPT itself: `ChatGPT-Account-ID`, `oai-did`, `oai-language`, `originator`, and `x-oai-*` / `x-openai-*`. Their raw values stay in per-tab `chrome.storage.session`. Cookies, `Authorization`, browser-managed `sec-*` headers, and arbitrary headers are not captured for replay. None of the retained values is returned to Rust, printed in diagnostics, or written to Chatarium's journal.

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
- first account-history page: `2026-10-03.003`
- exact C02 conversation read: `2026-10-03.001`

## Proof ladder

A successful result carries only safe proof metadata:

- extension version;
- desktop roundtrip (established when Rust receives the correlated result);
- exact ChatGPT tab found;
- MAIN-world execution;
- account-context presence;
- observed first-party request-context presence;
- original first-party HTTP status when observed;
- count of browser-local context headers selected for replay;
- request-profile identity;
- replay HTTP status.

Rust then adds parser/semantic proof. Exact-conversation synchronization does not become a complete success until the matching validated mirror is durably committed to the local journal.

A lower-level success must never be presented as a higher-level success.

## First-party request-context observation

The extension does not guess, hard-code, or ask Rust for the active ChatGPT account identifier or surrounding application request context.

For the exact ordinary-history resource, it passively observes ChatGPT's own first-party request headers, validates the account selector, copies only the narrow replay allowlist described above into per-tab extension session storage, and records the first-party HTTP completion status when available. Closing the tab or navigating the main frame clears both account and request context.

The replay path then requires that observed context. A history request fails closed with `first_party_request_context_unavailable` rather than falling back to a synthesized URL-plus-account-ID request. The extension-generated replay is excluded from observation so it cannot overwrite its own first-party evidence.

The 2026-10-03 HAR is important but limited evidence here: it established this global request shape/context, while the global list responses captured in that HAR were HTTP 429. Successful HTTP 200 conversation-list-like responses in the same HAR came from gizmo/project endpoints and are not treated as proof of ordinary global-history semantics.

This is Edge Bridge **0.2.0**, the single evidence-driven correction following the first 0.1.0 live run. That run proved extension transport, tab selection, MAIN-world execution, authentication, account context, HTTP 200, and parsing, but returned an unconfirmed empty global history.

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
