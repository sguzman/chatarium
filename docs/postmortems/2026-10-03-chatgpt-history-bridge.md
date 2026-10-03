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

### Stage K — stop decision

The project stopped iterating on Tampermonkey as a critical runtime transport.

Future browser integration will use a purpose-built Edge/Chromium extension with explicit instrumentation and a real browser integration harness.

Tampermonkey may remain useful for disposable experiments or capture helpers. It is not an acceptable production dependency for Chatarium account synchronization.

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
- a proper extension as the next implementation boundary.

No Edge-extension work begins until this postmortem and its rule changes are merged.
