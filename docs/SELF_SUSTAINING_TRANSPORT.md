# Self-sustaining ChatGPT transport contract

This document is a hard product and architecture contract for Chatarium.

It exists to prevent a specific form of architectural drift: replacing the intended native ChatGPT desktop client with an increasingly elaborate browser-puppeteering system and then calling the result "self-contained."

That is not the target.

## Product requirement

Chatarium's intended end state is a **self-sustaining native desktop client for ordinary ChatGPT conversation operation**.

Ordinary operation includes, at minimum:

- list the user's ordinary ChatGPT conversations;
- retrieve an existing conversation and its current branch/pagination state;
- create a conversation;
- continue an existing ordinary ChatGPT conversation rather than silently forking it into an unrelated local-only thread;
- send a user message;
- receive and stream the assistant response;
- preserve remote conversation/message identity needed for later synchronization;
- reconcile resulting remote state into Chatarium's durable local archive.

These operations must ultimately be performed by Chatarium's own native process through an empirically understood first-party ChatGPT protocol.

The availability or absence of a documented public API is **not** the project's viability criterion. Chatarium already has a protocol observatory precisely because important consumer-service behavior may need to be learned from first-party network evidence.

OpenAI's own first-party desktop clients are evidence that ordinary ChatGPT interaction is not inherently coupled to the website DOM. They do not, by themselves, prove which authentication/session mechanisms a third-party client can reproduce. That question must be answered empirically rather than used either as a guarantee or as an excuse to stop investigating.

## What counts as self-sustaining

Browser use is acceptable for:

- protocol investigation;
- passive network recording;
- one-time or occasional authentication/session bootstrap;
- login, MFA, CAPTCHA, explicit security consent, or another true account-holder boundary;
- recovery from an expired session when native renewal is not empirically available.

Browser use is **not** acceptable as the permanent transport for ordinary conversation traffic.

In particular, a design does not satisfy this contract if Chatarium must keep Chromium running and use a browser extension, CDP, Playwright, WebDriver, DOM automation, or page-context fetches for every ordinary:

- conversation-list read;
- conversation fetch;
- message send;
- response stream.

Hiding Chromium, launching it in the background, or wrapping it in a helper process does not make that design self-sustaining.

## Browser-dependency acceptance test

After any genuinely necessary authentication/session bootstrap:

1. terminate Chromium and all Chatarium browser-control sessions;
2. start or continue running Chatarium;
3. list ordinary remote ChatGPT conversations from the native process;
4. retrieve an ordinary existing ChatGPT conversation from the native process;
5. create a conversation from the native process;
6. send a user message into an ordinary existing ChatGPT conversation from the native process;
7. receive/stream the assistant response in the native process;
8. durably reconcile and render the resulting conversation locally;
9. verify that no browser process, extension, CDP session, Playwright session, or browser bridge participated in steps 3-8.

An occasional browser login/session bootstrap may still satisfy the contract.

A browser required for each ordinary read/write does not.

## Project viability gate

The focused transport investigation must eventually end in one of three explicit conclusions.

### VIABLE

Chatarium can perform ordinary authenticated ChatGPT reads and writes directly from its native process.

### VIABLE WITH AUTH BOOTSTRAP

A browser is required only for occasional login/session establishment, MFA/CAPTCHA, or equivalent identity/security boundaries. After bootstrap, the native process performs ordinary reads/writes independently.

### NOT VIABLE

Ordinary first-party conversation traffic proves materially browser-bound in a way that Chatarium cannot reproduce safely and reliably from the native process.

If the third conclusion is established after a focused evidence-based investigation, treat it as a **project viability failure**. Do not redefine success downward to "the browser bridge is reliable enough."

## Public API and auxiliary transports

Official Sign in with ChatGPT / Responses support may remain useful for:

- local-only Chatarium conversations;
- inference experiments;
- transport comparison;
- fallback capabilities that do not pretend to be the user's existing ordinary ChatGPT conversation.

It is not, by itself, the final replacement-client experience if it cannot operate on the user's ordinary ChatGPT conversation corpus with preserved remote identity.

Likewise, the absence of a public endpoint for a capability is not evidence that the capability is impossible. It means the project must rely on the evidence hierarchy below.

## Protocol evidence hierarchy

Transport work must use evidence in this order whenever practical:

1. existing HAR / Flight Recorder traces;
2. existing durable local raw mirror snapshots;
3. passive capture of normal first-party ChatGPT activity;
4. structural comparison/diffing across multiple real specimens;
5. minimal controlled experiments for one specific unresolved question.

Do not manufacture duplicate traffic merely because browser automation exists.

Do not design a new transport from guesses while relevant evidence already exists.

Do not tell the principal that HAR/CDP evidence is unnecessary when the target is undocumented first-party behavior.

## Existing local mirrors are protocol specimens

Durable raw local mirror snapshots are not merely UI content. They are private protocol evidence.

They may be inspected locally, without network traffic, to map the read-side conversation model, including where present:

- top-level conversation envelope;
- conversation identity;
- node/message mapping;
- current node;
- parent/child ancestry;
- roles and message IDs;
- content types;
- timestamps;
- model metadata;
- branch structure;
- pagination flags;
- hidden/internal node classes;
- reasoning/internal scaffolding;
- other structural fields.

Private message text must not be copied into committed fixtures merely to perform this analysis.

These read specimens do **not** establish the write protocol. Outbound message-send semantics require outbound traces or passive observation of real sends.

## Passive capture principle

Normal ChatGPT activity should be treated as valuable protocol evidence.

A purpose-built browser recorder may passively capture candidate first-party traffic generated by ordinary use and spool it locally for later analysis. A useful private spool may include:

- timestamp and ordering;
- method, URL/path/query structure, and initiator;
- request-body bytes for relevant candidate operations;
- response status/content type and relevant response-header names;
- response bodies for bounded candidate JSON/SSE traffic;
- timing and failure state;
- deterministic hashes/deduplication metadata.

The recorder must not create extra ChatGPT requests merely to populate the spool.

Reusable credentials, cookies, authorization values, and equivalent secrets must not be dumped into logs or committed evidence. If private request-context study is required, it stays in protected local evidence and is reduced to the minimum semantic facts needed for implementation.

The passive recorder is an **investigation and ingestion tool**, not automatically the permanent production transport.

## Why passive capture comes before more active browser machinery

The user's network can be highly unstable and has produced waves of 4xx responses. Active reloads, synthetic fetches, repeated bootstrap operations, or retries can both waste scarce successful traffic and make the observation surface harder to interpret.

Therefore:

- reuse traffic that the real client already paid to generate;
- cache useful candidate responses aggressively;
- deduplicate locally;
- prefer offline analysis of captured specimens;
- use active traffic only to answer a named unresolved protocol question;
- never confuse retry/backoff engineering with solving the transport architecture.

Remote-health/cooldown machinery is defensively useful, but it does not satisfy this contract.

## Current browser machinery is provisional

The existing Playwright Chromium profile, MV3 bridge, CDP discovery, exact-response capture, and related queue/controller work remain useful for:

- evidence gathering;
- regression experiments;
- ingestion of already-observed traffic;
- local-mirror bootstrap;
- compatibility diagnostics;
- authentication/session experiments.

They are **provisional infrastructure**, not the target transport architecture.

The fact that a browser-assisted path works does not close the native-transport question.

## Existing transport-independent work remains valuable

Replacing the remote transport does not invalidate the local architecture already built. Preserve:

- append-only durable journal;
- local/remote identity provenance;
- durable raw mirror snapshots;
- transcript projection;
- full/partial provenance semantics;
- offline reader;
- local archive search/filtering;
- integrity audit;
- backup/verification/restore;
- durable queue and reconciliation machinery.

These components should sit behind whatever native transport the protocol investigation ultimately proves viable.

## Anti-drift rule

Before adding new browser-control, retry, bridge, or remote-health infrastructure, ask:

> Does this materially advance native browser-independent ordinary ChatGPT conversation reads/writes, or produce protocol evidence needed to determine whether that is viable?

If the answer is no, the work requires explicit justification.

Do not keep accumulating browser-control infrastructure because it is already present.

Do not silently demote ordinary existing-conversation interoperability to an optional import feature.

Do not use "there is no public API" as a reason to avoid first-party protocol investigation.

## Required next investigation

Before substantial new remote-transport feature work, use existing evidence to map:

### Read side

- conversation-list surfaces and pagination;
- exact conversation retrieval;
- branch/current-node semantics;
- identity and versioning fields;
- session/account context required by the first-party client.

### Write side

- new-conversation creation;
- continuation of an existing conversation;
- outbound user-message envelope;
- parent/message/conversation identity;
- model/settings fields;
- streaming response protocol;
- completion semantics;
- stop/regenerate/edit/branch behavior where needed.

The write-side map should begin with existing HAR/Flight Recorder evidence and passive traces of ordinary user activity, not guessed requests.

## Engineering lesson

Chatarium previously over-invested in active Chromium-driven fetching and reliability before validating whether Chromium should be the long-term transport at all.

That produced useful local durability and browser-observation machinery, but it was the wrong ordering for the product's core viability question.

The corrective policy is:

> **evidence-first protocol reconstruction, then native transport; browser automation is an instrument, not the presumed product architecture.**

This document is the guardrail against repeating that mistake.
