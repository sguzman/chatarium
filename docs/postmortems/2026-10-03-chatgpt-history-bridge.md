# Postmortem: ChatGPT history interoperability and the Tampermonkey bridge

**Incident window:** 2026-10-02 through 2026-10-03  
**Scope:** existing ChatGPT conversation discovery/read interoperability in Chatarium  
**Status:** Tampermonkey is retired as a critical runtime transport. A purpose-built Edge extension is the next browser-integration architecture.  
**Related issues:** #88, #92, #94, #95, #96, #97, #98, #99, #100, #101

For the audit-oriented timeline and claim/proof matrix, see [the companion evidence ledger](2026-10-03-chatgpt-history-bridge-ledger.md).

## Executive summary

This effort consumed substantially more operator time than it should have because engineering repeatedly moved from partial evidence to implementation, then interpreted intermediate infrastructure signals as product success.

The most important early error was telling the operator that an Edge HAR was not necessary for the account-history interoperability goal. That judgment was wrong.

For a replacement ChatGPT client, public Sign in with ChatGPT documentation can establish the supported developer surface, but it cannot establish the private consumer-web protocol used by chatgpt.com for account conversation history. The existing Flight Recorder evidence also did not contain every literal/query/header/context property needed to reproduce the first-party request. A current HAR or equivalent CDP capture was therefore load-bearing evidence, not optional busywork.

The operator produced the HAR anyway. It immediately established facts the implementation had previously lacked, including the exact C02 query values `num_turns=10&include_has_versions=true`, a newer message-content shape, a concrete ordinary-history C01 request profile, current new-conversation request ordering, and later the crucial fact that first-party conversation-list requests carry `ChatGPT-Account-ID`.

After that, the project still made a second class of error: it built a Tampermonkey-based browser bridge with inadequate end-to-end observability. Several statuses were technically true but product-misleading:

- `listener ready` proved only that Rust had bound localhost;
- `browser authenticated` proved that a browser command reached an authenticated ChatGPT page;
- `0 recent chats` proved that one response parsed, not that the request represented the user's active ChatGPT account;
- green CI proved local protocol/state behavior, not live browser compatibility.

Those distinctions should have been explicit from the first bridge build.

The bridge then went through multiple transport/semantic revisions:

1. Tampermonkey `GM_xmlhttpRequest` long polling;
2. Site Access diagnosis;
3. direct page-context localhost fetch plus GM fallback;
4. safe account-context observation and proof diagnostics.

The v0.3 round trip demonstrated that the browser-to-desktop loopback could work. It then returned a semantically implausible empty history. Comparison against the HAR found the missing `ChatGPT-Account-ID` header. v0.4 attempted to preserve that account context in browser memory and fail closed without it, but the live bridge again failed to reach the listener. The exact cause of that regression was not established. At that point the architecture had demonstrated enough fragility that continuing to iterate on the userscript would have been irresponsible.

The result is not "nothing worked." A large amount of reusable infrastructure is valid and remains valuable:

- official Sign in with ChatGPT inference;
- durable local conversation storage;
- account-export import as backup/bootstrap;
- historical read-only browsing;
- exact C02 request evidence;
- remote identity and live-mirror durability;
- exact-ID validation before binding;
- safe active-branch transcript projection;
- typed loopback protocol;
- evidence-gated parsing;
- improved sidebar and diagnostics;
- recognition that account context is part of request parity;
- a clear separation between local, historical-import, and remote-backed identity.

The failure was the browser integration strategy and the evidentiary/QA discipline around it.

## 1. Product target

The product target was and remains:

> Chatarium should be able to open the user's actual ChatGPT account conversations, preserve their remote identity, maintain a durable local mirror, and eventually write into the same remote conversation when that mutation protocol has been separately evidenced.

A permanently isolated local-only Responses client does not satisfy this target.

Account exports are useful for backup, bootstrap, and disaster recovery. They are not an acceptable runtime synchronization architecture.

## 2. Failure chronology

### Stage A — the incorrect "HAR is not needed" judgment

Before the decisive 2026-10-03 capture, engineering told the operator that an Edge HAR was unnecessary.

That was a substantive mistake.

At that point the project knew that:

- public Sign in with ChatGPT did not expose consumer account history;
- live compatibility therefore depended on consumer-web behavior;
- the Flight Recorder corpus had partial C01/C02 evidence;
- C02 query values were still unknown in older evidence;
- current request headers/account context had not been established.

Given those facts, a full current network capture should have been requested immediately or produced by an automated CDP harness.

Instead, the operator had to insist on collecting the HAR. This reversed the intended QA burden: the operator correctly identified the missing evidence while engineering discouraged collection of it.

**Permanent classification:** incident-level process failure.

### Stage B — the HAR proves it was necessary

The operator supplied an Edge 153/Linux HAR from a disposable ChatGPT test conversation.

The HAR produced several load-bearing discoveries:

- successful C02 existing-conversation read:
  `GET /backend-api/conversations/<id>?num_turns=10&include_has_versions=true`;
- exact query literal values and occurrence order;
- current C02 response envelope and a newly observed `thoughts` / `source_analysis_msg_id` content shape;
- ordinary-history C01 request profile:
  `/backend-api/conversations?...limit=20&order=updated&offset=0`;
- successful new-conversation prepare/send/rename behavior;
- no evidence for a second send into an already-existing conversation;
- later comparison showed first-party conversation-list requests include `ChatGPT-Account-ID`.

The raw HAR remained private and outside Git; sanitized structural evidence was committed.

This was exactly the kind of artifact the earlier judgment said was unnecessary.

### Stage C — account export importer: technically successful, product-incomplete

Issues #94 and #95 implemented:

- content-addressed account-export ingestion;
- stable local identity across repeated exports;
- preservation of remote identity and raw branch DAG;
- read-only historical browsing;
- active-branch reconstruction;
- hash validation;
- explicit historical provenance;
- no fake live-binding claim.

This work was good and remains useful.

The product error was treating it too close to the main solution. The operator correctly objected that imported history is a worse substitute for actual account synchronization.

The project then explicitly demoted export import to:

- backup;
- bootstrap;
- recovery;
- historical provenance.

It is not the runtime source of truth.

### Stage D — #96 live exact-conversation mirror

The HAR-backed C02 evidence enabled the first narrow live-read bridge.

Reusable successes:

- exact remote conversation ID is preserved;
- browser credentials remain in the browser;
- browser returns only typed result data;
- response remote identity must match expected identity before any binding event is appended;
- network work does not run on the egui render thread or persistence worker;
- live mirror events are durable and replayable;
- transcript projection follows active parent lineage and fails closed at unsupported boundaries;
- same-thread writeback remained disabled because it was not evidenced.

This slice was conceptually sound.

The problem was the chosen browser transport and insufficient instrumentation.

### Stage E — #97 conversation enumeration

The project then added an evidence-gated first page of account history:

- fixed limit 20;
- offset 0;
- ordinary, non-archived, non-starred history;
- no automatic pagination;
- no hidden retry after 429.

A new remote conversation could be:

1. discovered from the list;
2. fetched by exact ID through C02;
3. identity-validated;
4. given one stable local lineage;
5. stored as a durable live mirror.

This was the correct product flow.

However, the implementation still lacked proof that its browser request matched the first-party account context.

### Stage F — the sidebar and status UI failure

The first human QA surface was poor enough to obstruct diagnosis.

Observed defects included:

- fixed 236 px sidebar;
- status text colliding horizontally;
- controls extending outside the visible column;
- lower controls becoming unreachable;
- nested scroll areas trapping wheel input;
- "History bridge: listener ready" being presented close to a healthy connection state even though it proved only a local listener existed.

The operator described the result as a "textual war crime." That description corresponded to a real usability failure.

The UI was changed to:

- 320 px default sidebar;
- 260–440 px resizable bounds;
- one outer vertical scrollbar;
- no nested sidebar scroll traps;
- wrapped status details below labels;
- controls sized to available width.

**Permanent lesson:** diagnostics that cannot be read are failed diagnostics.

### Stage G — incorrect Site Access diagnosis

When the userscript visibly ran but Chatarium timed out, engineering attributed the failure to Chromium/Edge extension Site Access and instructed the operator to grant broader permission.

The operator then supplied a screenshot proving Tampermonkey was already configured for **On all sites**.

The diagnosis had been presented too confidently without first collecting account-specific proof.

This was another evidence-discipline failure:

- a plausible browser restriction was promoted to a likely root cause;
- the operator was asked to perform another configuration check;
- the check falsified the diagnosis.

The project must distinguish "possible cause" from "established cause" explicitly.

### Stage H — Tampermonkey 5.5.0 / Chromium 153 and v0.3 transport

The environment was identified as Tampermonkey 5.5.0 on Chromium/Edge 153, a combination with upstream reports of MV3/background GM networking stalls.

The bridge was changed so v0.3:

- preferred page-context fetch to localhost;
- used narrow CORS/preflight for `https://chatgpt.com`;
- retained GM transport only as fallback on unaffected environments;
- did not rely on the known-problematic GM path in the exact affected version combination.

The next QA result was:

`browser authenticated · 0 recent chats`

This was important.

It proved substantially more than previous runs:

- Rust listener existed;
- browser bridge command delivery occurred;
- browser executed an authentication probe;
- ChatGPT session was authenticated;
- history command executed;
- a JSON result crossed localhost back into Rust;
- Rust accepted the list envelope.

It did **not** prove:

- correct active account context;
- parity with the first-party request;
- correct semantic history result.

Engineering initially celebrated too early. The operator correctly rejected that as proof because the visible ChatGPT account clearly had rich history.

### Stage I — the missing account-context header

The v0.3 contradiction forced a direct HAR comparison.

The first-party conversation-list request included:

`ChatGPT-Account-ID: <private value>`

The bridge's request did not.

This was a concrete request-parity defect and a strong explanation for:

- authenticated session;
- HTTP-valid conversation-list response;
- semantically wrong empty account history.

This discovery should have happened before #97 was handed to human QA.

The reason it did not is that the original evidence extraction focused too narrowly on:

- path;
- safe query values;
- response shape;

and not enough on:

- context-bearing request headers;
- account/workspace selection;
- first-party request parity.

### Stage J — v0.4 proof instrumentation and regression

v0.4 attempted to fix both the semantic and observability failures:

- install at document start;
- observe first-party same-origin fetches;
- capture only `ChatGPT-Account-ID` in browser memory;
- never export/log/journal the raw account ID;
- require account-context proof before history/exact-conversation reads;
- expose safe proof fields:
  - transport;
  - account-context yes/no;
  - HTTP status;
  - parsed item count;
  - remote total;
- reject a false empty-list success without account context.

The next live QA did not reach that proof stage. The desktop reported that the userscript did not reach the loopback listener.

The exact reason for the v0.4 regression was **not established**. It would be wrong to invent one after the fact.

What was established was enough:

- small userscript revisions could move the critical path between "round trip works" and "no loopback traffic";
- behavior depended on page-world, extension-world, Edge, Tampermonkey, MV3/background lifetime, Local Network Access, CSP/CORS, and request monkey-patching interactions;
- CI could test syntax and local protocol behavior but not the authenticated browser integration itself;
- the human operator remained the effective integration test harness.

That is unacceptable for a core product dependency.

### Stage J2 — Edge 0.1.0 falsifies the reconstructed C01 semantics

The first purpose-built Edge bridge live run materially improved the evidence quality. It proved:

- extension service worker ↔ desktop typed roundtrip;
- exact ChatGPT tab selection;
- MAIN-world execution;
- authenticated session;
- observed account context;
- replay HTTP 200;
- Rust list parser success.

The result was still `items=0 · total=0`, which the new semantic gate correctly classified as `unconfirmed-zero` rather than success.

Re-auditing the raw 2026-10-03 HAR after this contradiction found an important error in the project's own earlier evidence description: the ordinary global `/backend-api/conversations?...limit=20&offset=0` request shape was captured, including `ChatGPT-Account-ID` and surrounding application headers, but both global-list responses in that HAR were HTTP 429. The HAR therefore established **request shape/context**, not successful C01 response semantics. HTTP 200 conversation-list-like bodies in the same HAR came from gizmo/project endpoints and cannot be promoted to account-wide ordinary-history evidence.

This matters because Edge 0.1.0 still reconstructed the request from the frozen URL plus the account selector. It did not replay the surrounding first-party application context.

The single permitted evidence-driven correction is Edge Bridge 0.2.0:

- observe the exact current first-party global C01 request;
- retain only a narrow browser-local allowlist of application-controlled headers;
- never retain cookies or authorization material;
- replay that observed context in the exact ChatGPT tab's MAIN world;
- return only safe proof metadata to Rust: request-context present, original first-party HTTP status when available, and context-header count;
- fail closed if first-party request context was not observed.

No claim is made yet that 0.2.0 fixes global history. It must pass automated checks before its one final live validation.

### Stage J3 — Edge 0.2.0 proves exact-request replay is not a reliable runtime dependency

Edge Bridge 0.2.0 passed its complete automated gate before the final allowed live validation.

The operator then tested it twice:

1. with the existing authenticated ChatGPT tab;
2. after fully reloading that tab.

Both runs failed at the same boundary:

```text
extension=0.2.0
transport=extension
tab=yes
account-context=yes
request-context=no
first-party-http=unknown
context-headers=0
profile=2026-10-03.003
first_party_request_context_unavailable
```

The second run is decisive for the architecture. The extension was alive, could identify the ChatGPT tab, and had valid account context, but the exact frozen ordinary-history C01 request did not appear and therefore could not seed request-context replay.

The project must not reinterpret this as another missing-header problem. The dependency itself was wrong: **a runtime history bridge cannot require the current ChatGPT frontend to emit one exact historical request shape on demand.**

Per the human-QA stop rule:

- no third 0.2 live run was requested;
- no broader `webRequest` permission experiment was requested;
- no DevTools/console inspection was delegated to the operator;
- no guessed replacement header set was added;
- issue #102 was closed as an abandoned architecture.

### Stage K — escalation to live CDP surface discovery

The project now has two retired critical-path browser architectures:

1. Tampermonkey transport;
2. Edge exact-C01 reconstruction/replay.

The next architecture is Edge Bridge 0.3, tracked by #103. It uses a bounded `chrome.debugger` attachment to the selected ChatGPT tab and observes the browser's **actual current successful first-party Network traffic**.

For one discovery operation it automatically reloads the tab, watches successful JSON `/backend-api/*` responses, reads completed candidate bodies through CDP, extracts only bounded typed conversation summaries and structural cursor evidence, then detaches.

This changes the epistemic direction:

```text
old:
historical endpoint assumption
    ↓
wait/reconstruct/replay
    ↓
hope current frontend semantics match

new:
current first-party successful traffic
    ↓
classify observed list-like surfaces
    ↓
prove pagination/completeness separately
    ↓
only then promote a ConversationList baseline
```

The `debugger` permission is deliberately more powerful than the retired `webRequest` observer and is therefore treated as an explicit architectural cost. It is bounded to the selected ChatGPT tab and discovery interval; raw cookies, authorization values, request-header sets, and browser storage are not returned to Rust.

Successful CDP candidate discovery is still **partial proof**. It does not establish account-wide completeness by itself.

Tampermonkey may remain useful for disposable experiments or capture helpers. Exact-C01 replay remains useful only as historical evidence. Neither is an acceptable production account-history synchronization dependency.

### Stage K2 — 0.3 live discovery succeeds; synthetic exact read becomes the next failed boundary

The first live validation of Edge Bridge 0.3 succeeded at its intended boundary.

Observed evidence:

- Edge displayed the expected extension-debugging banner;
- the bounded CDP session ran;
- Chatarium populated its remote-history sidebar with **85 real ChatGPT conversation summaries/titles** observed from current first-party traffic;
- the discovery result reached the desktop through the typed loopback and Rust parser;
- the previous `first_party_request_context_unavailable` boundary was eliminated rather than bypassed.

This is the first live success for automatic ChatGPT history-surface discovery.

The UI initially displayed `CHATGPT HISTORY · 85/85`. That was incorrect. The denominator was not observed; the UI merely fell back to the observed-item count when `remote_conversation_total` was unknown. The correct claim is:

```text
85 observed
account-wide coverage unknown
```

That presentation defect is now fixed. An `N/N` display is forbidden without real total/completeness evidence.

The same live run then clicked one discovered conversation. That action crossed into a **different downstream mechanism**: the older synthetic MAIN-world C02 GET. It timed out and the UI replaced the successful History bridge state with:

`PROOF FAILED during exact fetch: browser bridge timed out`

Two conclusions follow:

1. discovery remained successful; one mirror failure must not retroactively invalidate it;
2. the remaining synthetic C02 fetch should not be extended when a first-party navigation can produce the evidenced response directly.

The run also produced one OS-level `Application Not Responding` dialog for Chatarium during the browser-debugging interval. The exact cause is not established. It is recorded as a separate UI-responsiveness observation and must not be invented as a CDP root cause without evidence.

### Stage K3 — 0.4 moves exact mirroring to first-party CDP navigation

Edge Bridge 0.4, tracked by #104, applies the successful 0.3 principle to exact conversation mirroring.

Instead of constructing:

`GET /backend-api/conversations/<id>?num_turns=10&include_has_versions=true`

inside the page, Chatarium now creates a temporary background browser tab, attaches CDP before navigation, navigates that tab to `https://chatgpt.com/c/<id>`, and captures the exact successful first-party conversation response generated by ChatGPT itself.

The response still must pass the existing `2026-10-03.001` C02 parser and exact remote-ID validation before durable import. This is not a semantic shortcut; it is a transport/capture correction.

The active status model is now explicitly split:

```text
History discovery:
  discovering
  discovered / N observed
  coverage unknown
  failed

Per-conversation mirror:
  discovered
  mirroring
  validating
  persisting
  fully mirrored locally
  mirrored locally / partial
  failed / retry
```

A per-conversation mirror failure is never allowed to erase a successful discovery proof.

### Stage K4 — live 0.4 regression proves passive discovery was opportunistic

The first 0.4.0 live run and the subsequent 0.4.1 repair run both regressed history discovery to zero observed conversation items.

0.4.1 had restored the exact 0.3 discovery listener and wait sequence, so the repeated zero result disproved the earlier diagnosis that the regression was fully explained by debugger-listener multiplexing or altered body-grace timing.

The key evidence from the 0.4.1 failure was:

```text
responses=129
backend-200=29
json-candidates=1
candidates=1
current-pass-items=0
best-surface=/backend-api/conversation/init (0)
```

The 85-chat 0.3 success therefore remained valid, but the passive algorithm was not deterministic: it only saw useful history when the frontend itself happened to emit a list-bearing request during the bounded reload window.

Re-auditing the private HAR identified the deterministic surface that best explains the successful 0.3 observation:

```text
GET /backend-api/gizmos/snorlax/sidebar
  ?conversations_per_gizmo=5
  &limit=20
  &owned_only=false
→ HTTP 200
→ ~185 KiB JSON
→ nested conversations.items
→ nested and top-level cursors
```

The HAR also records subsequent top-level cursor pagination requests on that same sidebar surface.

Edge Bridge 0.4.3 therefore changes the recovery policy without changing the proven passive path:

- the full 0.3 passive classifier/listener remains frozen;
- if passive discovery finds conversations, no recovery request is issued;
- if passive discovery finds zero, the extension reuses browser-local application context observed from ordinary first-party backend traffic;
- it performs one bounded MAIN-world GET for the successful sidebar surface;
- the response is reduced through the same conversation-summary classifier;
- raw application-context headers remain browser-local;
- Rust receives only safe proof metadata, typed summaries, and non-secret error state.

This is the first post-0.3 repair based on a **successful captured list response**, rather than another inference from an absent request.

### Stage K5 — 0.4.3 synthetic sidebar bootstrap is rejected with HTTP 403

The 0.4.3 live run produced a decisive result:

```text
account-context=yes
app-context-headers=10
sidebar-bootstrap=yes
sidebar-http=403
sidebar-items=0
sidebar-error=remote_http_status
```

The recovery request executed and had browser account context, but the server rejected the synthetic MAIN-world request. This is not an operator or parser failure. It falsifies synthetic sidebar replay as a production recovery mechanism.

The mistake repeated an already-known failure mode: reconstructing a private request is not equivalent to letting the first-party frontend originate it.

0.4.4 removes the synthetic sidebar fetch entirely. It instead uses a cache-bypassing CDP reload followed by bounded programmatic scrolling of actual ChatGPT navigation surfaces while CDP remains attached. The site therefore owns request construction, headers, anti-abuse context, and any internal request wrapper behavior. Chatarium only stimulates the UI and observes the resulting first-party responses.

Permanent rule added by this incident: a successful HAR response proves a response shape, but does not by itself authorize synthetic runtime replay when the site may attach unobserved or dynamic request context.

### Stage K6 — 0.4.6 live run exposes a circular UI-stimulus gate

The 0.4.6 live run established that the bridge transport itself was healthy:

```text
authentication=true HTTP=200
responses=147
backend-200=34
candidates=2
current-pass-items=0
stimulus-attempts=4
stimulus-targets=0
stimulus-steps=0
chat-links=0->0
```

This is not an MV3-worker failure, authentication failure, or lack of browser traffic. Discovery attached successfully and observed substantial first-party traffic.

The defect was in the 0.4.4 UI-stimulus eligibility rule. It rejected every scrollable element that contained zero conversation/project links before it calculated whether the element belonged to `nav`, `aside`, or `[role="navigation"]`.

That made the recovery path circular:

```text
history links must already exist
        ↓
surface becomes eligible for scrolling
        ↓
scrolling is supposed to cause lazy history links to load
```

0.4.7 changes only that gate. Scrollable navigation-owned surfaces are eligible even with zero current history links; unrelated scroll surfaces still require real conversation/project links. Synthetic history API replay remains retired. Exact mirroring, 429 recovery, and the MV3 keepalive are unchanged.

A package invariant now rejects reintroducing the old unconditional zero-link skip.

### Stage K7 — 0.4.7 fixes target selection but overruns the bridge timeout

The first 0.4.7 live run changed the failure shape again:

```text
authentication=true HTTP=200
discover_history_surfaces timeout=30000ms
next probe_auth timeout=5000ms
```

The second authentication timeout was downstream damage, not evidence that authentication suddenly failed. The extension bridge loop executes one command at a time and awaits `execute(command)` before polling for the next command. Rust abandoned the discovery command after 30 seconds, but the extension was still inside the injected history stimulus, so the next `probe_auth` could sit uncollected until its own shorter timeout expired.

The new 0.4.7 eligibility rule made navigation surfaces available for stimulus, but exposed a duration bug that had been latent in the 0.4.4 design. One stimulus could visit up to three targets, perform up to 28 delayed scroll steps per target, and add bottom-of-list waits while lazy content continued extending the scroll range. In the lazy-loading case the operation could exceed the desktop's discovery result wait.

0.4.8 repairs the contract instead of increasing the discovery timeout blindly:

- one injected history stimulus has a hard 8-second wall-clock budget;
- the extension still uses the same first-party reload + UI-stimulus architecture;
- the package test calculates the worst-case discovery budget, including retries and grace time;
- the calculated budget must retain at least five seconds of margin before Rust's discovery timeout;
- Rust's authentication result wait now exceeds the extension's page-fetch timeout, preventing the same abandoned-command mismatch on auth.

Permanent rule: every cross-process command must have a producer-side execution bound strictly below the consumer-side wait, and that relationship must be machine-checked.

### Stage K8 — stop patching; restore and freeze the last live-proven discovery runtime

The 0.4.7/0.4.8 sequence established that continuing to mutate discovery under live human QA was itself the process failure.

The recovery therefore changed from "find the next plausible fix" to a mechanical Git-history reconstruction.

The last pre-0.4 repository state containing the 0.3 discovery implementation is:

```text
dca678f95442635cfc729d02c4ac612510d7efe7
```

That lineage contains the implementation that had already produced the live 85-conversation discovery result before exact-conversation mirroring work began.

A direct file comparison against current `main` found:

```text
history-discovery.mjs:
  baseline blob = 9d835480e0af99d4dc769623d5dc6e9574319917
  current blob  = 9d835480e0af99d4dc769623d5dc6e9574319917
  result        = identical

service-worker discovery listener:
  result        = drifted after 0.3

discoverHistorySurfaces:
  result        = drifted substantially after 0.3
```

The classifier was therefore not the thing that had been repeatedly destroyed. The runtime orchestration around it was.

0.4.9 restores the exact 0.3 discovery debugger listener and exact 0.3 `discoverHistorySurfaces` body from the known-good commit. It also restores the Rust-side discovery proof contract to the 0.3 field set and removes stimulus/cache-bypass fields from discovery UI diagnostics.

The active discovery path again consists only of:

```text
attach debugger
→ Network.enable
→ chrome.tabs.reload(tab.id)
→ bounded passive observation
→ response-body classification
→ detach
```

No sidebar replay, cache bypass, CDP Page.reload, programmatic sidebar scrolling, request-context harvesting, or stimulus retry logic remains inside discovery.

Later features are isolated instead of being allowed to rewrite that boundary:

- exact-conversation mirroring keeps a separate debugger listener/session map;
- the MV3 keepalive remains a transport-lifecycle concern only;
- the durable last-known discovery cache remains a desktop persistence concern only.

The exact 0.3 discovery listener/command and classifier are now copied into immutable repository fixtures. CI checks the active implementation against those fixtures. Future mirror/local-viewer work is not allowed to alter discovery accidentally.

Permanent process rule from this stage:

> A subsystem that has passed live validation becomes a frozen compatibility boundary. Later work must compose around it. If a change to that boundary is truly necessary, it requires a deliberately updated baseline plus its own evidence, not incidental edits while implementing another feature.

Human browser QA is suspended during this reconstruction. Repository/CI work must be exhausted before another live operator validation is requested.

## 3. Where engineering/assistant behavior failed

### F1 — discouraging the HAR

This was the clearest process error.

When the target depends on an undocumented first-party web protocol, current first-party network evidence is presumptively necessary. The burden is on engineering to prove that existing evidence is sufficient, not on the operator to argue for a HAR.

### F2 — implementing beyond the evidence envelope

The project correctly evidence-gated C02 query literals, but later treated C01 request reproduction as sufficiently specified before verifying context-bearing request headers.

A request is not equivalent because method/path/query match.

### F3 — confusing infrastructure milestones with product success

Examples:

- listener bound != browser connected;
- browser connected != authenticated page;
- authenticated page != correct account;
- HTTP 200 != semantically correct account result;
- parse success != synchronization;
- CI green != authenticated browser compatibility.

The UI must never collapse these into one green indicator.

### F4 — lack of falsification checks

The app accepted `total=0` even though the operator could visibly see many conversations in the first-party sidebar.

That should have been treated as a contradiction requiring investigation, not a plausible success.

### F5 — observability arrived after human QA

The bridge should have shipped its first QA build with:

- command sequence;
- transport selected;
- browser tab found;
- auth status;
- account-context status;
- request attempt;
- HTTP status;
- parser status;
- item count;
- remote total;
- last failure stage.

Instead, several rounds depended on screenshots and inference.

### F6 — overconfident causal diagnosis

"Site Access is probably blocking localhost" was plausible, but not established. The operator's screenshot falsified it.

Future diagnostics must use language such as:

- observed;
- established;
- likely;
- possible;
- unknown.

No possible cause becomes a root cause without evidence.

### F7 — too many architecture pivots under human QA

The operator was asked to evaluate multiple bridge generations while the architecture itself was still unstable.

Human QA should verify a finished integration boundary, not act as an interactive debugger for successive transport hypotheses.

### F8 — userscript chosen for a critical path without a stop rule

Tampermonkey was attractive because it was fast to prototype and could run inside an authenticated page.

That was reasonable for an experiment.

It became unreasonable when the project continued treating it as the likely permanent bridge despite:

- background transport instability;
- page/extension execution-world complexity;
- Local Network Access behavior;
- missing first-party context;
- weak observability;
- repeated environment-specific regressions.

A prototype must have explicit promotion criteria and explicit retirement criteria.

### F9 — UI QA was not performed before handing it to the operator

The clipped/nested-scroll sidebar was visible from a simple constrained-width run.

That should not have reached the operator.

### F10 — existing process documents were insufficiently enforced

The repository already said:

- capture before adapting;
- human QA should be rare;
- protocol mismatch must fail visibly;
- automation should do mechanical diagnostics.

Those rules existed as prose, but there was no hard gate saying "do not ask for live QA until proof instrumentation exists" or "do not implement consumer-web parity without a current request-complete capture."

This postmortem adds those gates.

## 4. What succeeded and remains reusable

### S1 — official inference path

Sign in with ChatGPT and the Responses-based inference path work and remain the supported base layer for:

- account authentication;
- plan entitlement;
- model discovery;
- inference.

It is not account-history synchronization.

### S2 — local durability

Chatarium's append-oriented local store, draft/message durability, replay, and crash-oriented architecture remain valid.

### S3 — account-export bootstrap

#94/#95 remain valuable for:

- offline archive;
- disaster recovery;
- historical provenance;
- corpus bootstrap;
- branch preservation.

They are no longer treated as the runtime sync architecture.

### S4 — HAR-derived protocol evidence

The 2026-10-03 HAR materially advanced the project and should remain a model for evidence collection.

### S5 — C02 exact read profile

The current evidence-backed C02 request remains:

`GET /backend-api/conversations/<id>?num_turns=10&include_has_versions=true`

The parser and revision-gated response handling remain useful for the extension architecture.

### S6 — remote identity discipline

The implementation correctly refuses to bind fetched remote content to a local lineage unless the response proves the expected remote conversation identity.

### S7 — live-mirror durability and transcript projection

The store-side live mirror, replay, current-node/parent active-branch projection, pagination uncertainty, and provenance rendering remain reusable independent of browser transport.

### S8 — typed localhost protocol

The localhost server's narrow command/result protocol is reusable by a proper extension. The extension should not become an arbitrary HTTP proxy.

### S9 — no guessed same-thread writeback

Despite pressure to reach parity, the project did not fabricate writeback support. The HAR did not contain a controlled second send into an existing conversation, so same-thread mutation remains unestablished.

### S10 — CI discipline

CI repeatedly caught:

- formatting drift;
- lockfile drift;
- test type mismatches;
- fake-browser race conditions;
- stale expectations.

CI was useful. Its limitation was scope: it could not certify authenticated live-browser behavior.

## 5. Exact integrated milestones

The important integrated heads from this incident were:

- `45d0b366...` — HAR-backed C02 evidence landed;
- `d140b106...` — live exact-conversation mirror path integrated;
- `c9048684...` — account-history enumeration and direct remote mirror UI integrated;
- `d112ba79...` — sidebar/diagnostic UX repair;
- `4be127f4...` — dual page-loopback / GM bridge transport;
- `98077f1a...` — account-context proof and safe diagnostics.

These commits document useful intermediate states. They do not imply that the final Tampermonkey integration achieved the product goal.

## 6. Evidence ladder for future browser integration

Every future browser integration must expose these states independently.

1. **desktop-listener-ready**
   - local server bound;
   - proves nothing about browser state.

2. **extension-loaded**
   - extension service worker/content surface is alive.

3. **chatgpt-tab-found**
   - exact `https://chatgpt.com` tab identified.

4. **browser-desktop-roundtrip**
   - extension can exchange one nonce-bearing typed command with Chatarium.

5. **page-main-world-execution**
   - a minimal function executed in the actual ChatGPT page main world.

6. **chatgpt-session-authenticated**
   - a first-party-safe auth probe succeeded.

7. **account-context-observed**
   - active account/workspace context required by first-party requests is available.

8. **request-parity-established**
   - method/path/query/context-bearing headers/body/credentials behavior matches captured first-party evidence for the target operation.

9. **remote-http-success**
   - target request returned expected status/content type.

10. **schema-validated**
    - result passes the evidence-gated parser.

11. **semantic-sanity-validated**
    - result is consistent with known visible/account state and invariants.

12. **durable-local-mirror**
    - exact remote identity was verified and snapshot committed.

No status may skip levels or summarize a lower level as a higher-level success.

## 7. Mandatory request-parity checklist

Before reproducing any private first-party ChatGPT request, compare:

- HTTP method;
- scheme/host/path;
- query keys;
- query values;
- query order/duplication where observed to matter;
- content type;
- request body shape;
- credentials mode;
- origin/referrer behavior where relevant;
- context-bearing headers;
- account/workspace/project identifiers;
- client/build identifiers when they may gate behavior;
- required challenge/Sentinel material;
- request ordering/dependencies;
- expected status;
- response content type;
- response schema;
- pagination metadata;
- identity echoes;
- first-party behavior under 401/403/429.

Headers must be classified individually as:

- required semantic context;
- required anti-abuse/challenge context;
- incidental telemetry;
- credential/private;
- unknown.

Unknown context is not silently omitted.

## 8. Mandatory HAR/CDP gate

For undocumented consumer-web integration, implementation may proceed beyond scaffolding only when one of these exists:

- current raw HAR from the exact target flow;
- automated CDP capture of the exact target flow;
- equivalent first-party network trace with request and response evidence.

DOM observation alone is insufficient for network parity.

A previous snapshot may be reused only after documenting why frontend/browser revision drift cannot affect the target property.

If current evidence is missing, engineering must say:

> Current evidence is insufficient; capture is required.

It must not tell the operator that a HAR is unnecessary merely because collecting one is inconvenient.

## 9. Raw evidence policy

Raw HAR/CDP evidence is private and may contain credentials, account/device identifiers, signed URLs, and conversation data.

Rules:

- never commit raw HARs;
- preserve a private hash and source metadata;
- sanitize into narrow structural evidence;
- do not manually destroy the raw evidence before sanitization;
- record which sensitive fields were removed;
- preserve safe literal values when they are necessary for protocol parity;
- preserve enough request-header classification to avoid repeating the `ChatGPT-Account-ID` omission.

## 10. Contradiction and semantic sanity rules

A structurally valid response is not sufficient.

Examples that must fail or warn loudly:

- remote history total is zero while first-party UI visibly shows ordinary conversations;
- response conversation ID does not match requested ID;
- account context is absent when first-party evidence requires it;
- pagination says older content exists but projection silently presents the page as complete;
- authenticated probe succeeds but target endpoint returns account-context-dependent emptiness;
- status says connected while no successful browser-desktop roundtrip has occurred.

A contradiction is evidence. It must not be normalized away.

## 11. Observability-before-QA rule

No new live browser integration may be handed to the operator unless the UI or generated diagnostic bundle can answer, without DevTools:

- which integration version is running;
- which transport is active;
- whether the browser component is alive;
- whether the exact ChatGPT tab was found;
- whether main-world execution worked;
- whether authentication worked;
- whether required account context was available;
- exact safe target request profile/revision;
- HTTP status;
- parser result;
- item/message count;
- remote total/pagination signal;
- last failure stage;
- correlation ID for the run.

The operator must not be asked to infer these from "it looks connected."

## 12. Human QA budget and stop rule

A browser architecture gets at most:

- one focused live validation after automated/local tests;
- one evidence-driven correction if that validation reveals a previously unknown concrete variable.

If the second live validation fails at the same architectural boundary, stop and escalate the architecture rather than asking for another speculative manual test.

Exceptions require a written justification in the issue before asking the operator.

This incident exceeded that budget.

## 13. Userscript promotion and retirement rule

Userscripts are appropriate for:

- disposable protocol experiments;
- temporary observation;
- small UI instrumentation;
- capture helpers.

They are not promoted to critical runtime transport unless they pass:

- stable browser-version matrix;
- deterministic desktop roundtrip;
- explicit execution-world model;
- reliable background lifetime;
- request-context parity;
- end-to-end integration harness;
- sufficient diagnostics to avoid manual debugging.

The Chatarium account-history userscript did not meet those criteria and is retired from the critical path.

## 14. UI QA gate

Before any diagnostic/control sidebar is handed to a human:

- test minimum supported width;
- test a narrow window;
- test enough content to force vertical overflow;
- verify every control remains reachable;
- verify status text wraps;
- prohibit nested scroll regions that trap access to lower controls unless intentionally designed;
- verify degraded/error messages remain readable.

A broken diagnostic surface invalidates the QA run because the operator cannot reliably observe the state.

## 15. Edge extension entry criteria

The replacement Edge extension must not repeat this incident.

Before first human QA it must have automated or self-diagnostic proof for:

- extension installed/version;
- extension service worker alive;
- exact ChatGPT tab discovery;
- localhost roundtrip;
- MAIN-world execution;
- first-party request observation;
- safe account-context availability;
- target request execution;
- target response status/schema;
- safe trace export.

The first QA build must include a visible diagnostic matrix, not one aggregate status line.

The extension should reuse the existing typed loopback, C02 parser, remote-identity validation, live-mirror store, and transcript projection rather than rewriting those layers.

## 16. Open questions

The following remain genuinely unresolved:

- exact cause of the v0.4 live loopback regression;
- complete first-party context needed beyond `ChatGPT-Account-ID` for every history operation;
- pagination behavior beyond the first evidenced page;
- archived/starred/special-origin enumeration;
- same-thread writeback protocol for an existing conversation;
- behavior under account/workspace switching;
- whether a normal extension localhost transport is sufficient everywhere or Native Messaging is eventually preferable.

These are open questions, not implementation facts.

## 17. Final accountability statement

The operator was correct on two key points that engineering resisted:

1. a current HAR was needed;
2. the successive "working" bridge statuses did not prove the feature worked.

The cost of those mistakes was repeated manual testing, repeated architecture revisions, and avoidable frustration.

The permanent response is not "be more careful." It is:

- mandatory network evidence before undocumented protocol reproduction;
- proof-level status instead of aggregate green lights;
- semantic contradiction checks;
- diagnostics before human QA;
- explicit architecture stop rules;
- userscripts limited to prototype/observation roles for this critical path;
- escalating browser architecture when the documented stop condition fires.

The purpose-built Edge extension proved that extension transport itself is viable, but its exact-request replay strategy was also falsified. The active implementation boundary is now bounded CDP observation of current first-party traffic, not another replay attempt.
