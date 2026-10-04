# Evidence ledger: 2026-10-03 ChatGPT history bridge incident

> **Current policy note (2026-10-04):** this ledger is historical evidence and its Edge/Tampermonkey references describe the incident as it occurred. The current browser-QA architecture is Playwright-managed bundled Chromium with a dedicated persistent user-data directory; personal Microsoft Edge is outside the automation boundary. Do not reinterpret historical Edge observations as current QA instructions.

This appendix records the concrete sequence of claims, evidence, QA observations, and corrected conclusions from the ChatGPT-history interoperability incident.

It is intentionally redundant with the narrative postmortem. The purpose is auditability.

## Legend

- **Observed** — directly demonstrated by capture, runtime output, screenshot, or durable code/test result.
- **Inferred** — plausible explanation consistent with evidence but not directly established.
- **Disproved** — later evidence contradicted the earlier claim.
- **Retained** — implementation/result remains useful.
- **Retired** — implementation is preserved only as historical/prototype evidence.

## Timeline

| UTC time / stage | Action or claim | Evidence available at the time | Outcome | Final classification |
| --- | --- | --- | --- | --- |
| Before 2026-10-03 02:58 | Engineering told the operator an Edge HAR was not needed for the current history-sync work. | Public SIWC limits; partial Flight Recorder C01/C02 evidence; no current request-complete trace. | Operator collected the HAR anyway. | **Disproved. Process failure.** |
| 2026-10-03 02:58 | Operator supplied Edge HAR from a disposable ChatGPT chat/settings session. | Raw first-party network capture. | Exact C02 query values, current response shape, C01 request profile, current create/send/rename sequence, and later account-context header evidence became available. | **Observed. Load-bearing evidence.** |
| #94 / #95 | Account export importer + read-only historical browser. | Export JSON and durable local-store tests. | Worked as archive/bootstrap, not live account sync. | **Retained, but product role demoted.** |
| `f13adab3...` → `45d0b366...` | HAR-backed C02 evidence landed. | HAR + sanitized protocol snapshot. | Exact `num_turns=10&include_has_versions=true` and content drift recorded. | **Observed / retained.** |
| #96 / `d140b106...` | Imported lineage can be upgraded to an exact-ID live mirror through browser-held session. | C02 evidence; typed loopback tests; store/parser tests; CI. | Store/durability/projection path became valid; browser transport not yet product-proven. | **Partially observed / retained core.** |
| 2026-10-03 04:12 | First live build screenshot showed local chat, SIWC connected, History bridge listener ready, but no account history. | Desktop screenshot. | Listener existence was visible; browser bridge connectivity was not proved. | **Observed lower-level state; earlier wording overclaimed.** |
| #97 / `c9048684...` | Add first-page account-history enumeration and direct remote mirror creation. | Composite C01 evidence + local fake-browser tests + CI. | Code path existed, but first-party account context had not been fully reproduced. | **Implementation retained; parity claim incomplete.** |
| 2026-10-03 04:50 | Operator showed userscript running, history bridge timeout, and severely broken sidebar. | Desktop/browser screenshots. | Browser bridge had not reached listener; sidebar diagnostics were unreadable/clipped. | **Observed failure.** |
| #98 / `d112ba79...` | Sidebar/diagnostic UX repaired; Site Access proposed as likely timeout cause. | Chromium/Tampermonkey permission model knowledge; no user-specific permission evidence yet. | UI fix worked; causal diagnosis remained speculative. | **UI retained; root-cause claim later disproved.** |
| 2026-10-03 05:09 | Operator supplied Tampermonkey extension settings screenshot showing Site Access = On all sites. | Direct screenshot of target environment. | Broad Site Access was already enabled. | **Disproved previous permission diagnosis.** |
| #99 / `4be127f4...` | v0.3 page-loopback transport added; GM fallback de-emphasized for TM 5.5.0 + Chromium 153. | Upstream instability reports + local transport tests. | Next live run successfully round-tripped. | **Transport strategy partially observed.** |
| 2026-10-03 05:32 | Desktop reported `browser authenticated · 0 recent chats`. | Live target screenshot. | This proved loopback command delivery, browser execution, authenticated probe, list command/result roundtrip, and parser acceptance. It did not prove correct account history semantics. | **Observed transport success + semantic failure.** |
| Post-v0.3 HAR comparison | Engineering compared the reproduced list request to first-party HAR. | Raw HAR request headers. | First-party conversation-list request contained `ChatGPT-Account-ID`; bridge request did not. | **Observed missing request context.** |
| #100 / `98077f1a...` | v0.4 attempted browser-local account-context observation and safe proof diagnostics. | HAR header evidence + local tests/CI. | Design failed closed in local tests; next live run did not reach listener. | **Implementation experiment; live result failed.** |
| 2026-10-03 05:58 | Operator screenshot showed userscript did not reach loopback listener. | Live target screenshot. | Critical transport regressed before account-context proof could be evaluated. | **Observed failure. Exact cause unknown.** |
| 2026-10-03 06:03 | Operator required complete failure/success documentation before further browser work. | Entire incident history. | Extension implementation halted pending documentation. | **Current gate.** |

## Exact integrated heads

These heads are useful historical checkpoints, not equivalent claims of product success.

| Integrated head | Meaning | Keep? |
| --- | --- | --- |
| `45d0b366b6ec020eef471be6ee814045b456da58` | HAR-backed C02 evidence baseline integrated. | Yes. |
| `d140b10690b9690c107d50e04e6bda6be0c8581a` | Exact-ID live mirror/store/UI pipeline integrated. | Yes; transport-independent parts are reusable. |
| `c90486843b1f2aed0994909676bffeb39e08b06d` | First-page history enumeration and discovered remote mirror path integrated. | Request/parser/store pieces reusable; browser parity incomplete. |
| `d112ba794b42419274a39c2615a4e8de0358f4fd` | Sidebar/diagnostic UI repair integrated. | Yes. |
| `4be127f46229a849eabbb9819e841f4e46d2a749` | v0.3 dual/page loopback bridge integrated. | Historical prototype evidence; one live roundtrip succeeded. |
| `98077f1ab60ec7f541e199cfeb77621db870de3d` | v0.4 account-context proof logic integrated. | Historical prototype evidence; live bridge failed before proof stage. |

## Claim/evidence matrix

| Claim | What would prove it | What we actually had | Verdict |
| --- | --- | --- | --- |
| "Rust history listener is ready" | Local bind succeeds. | Local bind. | True but narrow. |
| "Browser bridge is connected" | Browser↔desktop nonce roundtrip. | Not present in earliest UI. | Early UI was misleading. |
| "Browser is authenticated" | Browser executes auth probe and gets positive session evidence. | Achieved in v0.3. | Proven for that run. |
| "History request executed" | Browser returns target request status/result. | Achieved in v0.3. | Proven for that run. |
| "History request represents the user's active account" | Required first-party account/workspace context is present and request parity is established. | Missing in v0.3. | Not proven. |
| "0 recent chats is a correct result" | Request parity + semantic consistency with visible first-party account. | Visible account had rich history; account header missing. | False / contradicted. |
| "Tampermonkey Site Access caused timeout" | Target permission state shown restrictive or request succeeds after permission change. | Operator later showed On all sites already enabled. | Disproved. |
| "Tampermonkey can never reach localhost here" | Repeated failure with all relevant transports/permissions. | v0.3 successfully round-tripped. | False. |
| "Tampermonkey is stable enough for production bridge" | Repeatable roundtrips across revisions/browser restarts plus integration harness. | v0.3 worked, v0.4 regressed to no traffic. | Not established; architecture retired. |
| "C02 exact read semantics are known" | Current capture with exact request + successful response. | 2026-10-03 HAR. | Proven for captured revision. |
| "Same-thread writeback is known" | Controlled second send into an existing thread with request/response capture. | Not captured. | Explicitly unproven. |

## Operator burden ledger

The incident imposed manual work that engineering should have prevented or compressed:

- operator argued for and produced the HAR after being told it was unnecessary;
- operator repeatedly updated/reloaded the userscript;
- operator repeatedly pulled/rebuilt/relaunched Chatarium;
- operator kept authenticated ChatGPT tabs open for live experiments;
- operator supplied multiple screenshots to establish whether the bridge/UI was working;
- operator inspected Tampermonkey extension permission state;
- operator disproved the Site Access diagnosis;
- operator had to judge whether "authenticated · 0 recent chats" was credible because the app lacked proof diagnostics;
- operator endured a broken/clipped sidebar during a diagnostic run;
- operator became the effective cross-version Edge/Tampermonkey integration harness.

This is precisely the failure mode the Human QA policy is supposed to prevent.

## What the correct path should have been

The lowest-burden path, in hindsight, was:

1. State the product target precisely: real existing ChatGPT account history, not export-only history.
2. Immediately request or automate one current request-complete C01 + C02 HAR/CDP capture.
3. Sanitize and classify the complete request, including context-bearing headers.
4. Build store/parser/durability layers from that evidence.
5. Treat Tampermonkey as a short-lived protocol prototype only.
6. Before first human QA, add a proof matrix for every browser/desktop boundary.
7. Run one focused live validation.
8. If the browser prototype failed again after one concrete correction, replace it with an extension.
9. Only then expose the account-history UI as a product feature.

That path would have avoided most of the repeated manual QA.

## Facts still unresolved

Do not rewrite these as known later:

- exact cause of the v0.4 no-loopback regression;
- whether the missing account header alone explains the v0.3 zero-history response;
- all first-party headers/context required under every account/workspace state;
- deep pagination behavior;
- archived/starred/special-origin semantics;
- same-thread mutation/writeback;
- whether Native Messaging is eventually needed after a proper extension;
- behavior when the user switches ChatGPT account/workspace while Chatarium is running.

## Stop condition

This ledger closes the Tampermonkey account-history experiment.

No further live QA request may be made for `account-bridge.user.js` as the production history transport.

The next live QA request must come from a different, explicitly instrumented browser architecture and must satisfy the gates in:

- `docs/DEVELOPMENT.md`;
- `docs/HUMAN_QA.md`;
- `docs/RELIABILITY.md`;
- `docs/CAPTURE_HARNESS.md`;
- `protocol/CAPTURE_PLAYBOOK.md`;
- the narrative postmortem.
