# Local Inference Contract

Status: ACTIVE GATE, 2026-10-05.

This document defines the machine-readable boundary between Chatarium's local
conversation substrate and higher-level context, behavior, lifecycle, routing,
and orchestration work.

The contract file is:

`local-inference-contract.json`

It is derived from:

`siwc-capability-probes.json`

The probe report is evidence. The contract is a deterministic interpretation of
that evidence plus Chatarium's already-established local invariants. The
contract is not an independent source of capability truth.

## Why this exists

Behavior code must not rediscover inference constraints in the middle of a
context-composition, worker-lifecycle, or routing implementation. It also must
not infer support from general Responses documentation, a model name, an older
account, or a stale probe run.

The contract gives downstream code one narrow admission rule:

> Empirical remote capabilities may be consumed only when the contract is
> `ready` for the currently active SIWC profile and selected model.

The desktop module `apps/desktop/src/local_inference_contract.rs` owns that
derivation and the typed `ready_for(profile, model)` consumer guard.

## Evidence scope

Every derived contract records:

- the DevKit renderer-safe local `profileId`;
- the selected model slug;
- the timestamp of the probe evidence;
- the timestamp at which the contract was derived;
- the full sanitized empirical classification matrix.

The account email, access token, refresh token, ID token, cookies, and raw
credential state do not belong in this artifact.

Changing profile or model invalidates readiness for consumers even if the
capability names happen to look identical.

## Contract layers

### Local guarantees

These come from Chatarium's implemented architecture rather than empirical route
probing:

- conversation context ownership is local;
- local conversations are isolated unless Chatarium explicitly routes context;
- server response storage is disabled;
- streaming is required;
- continuation resends required local context;
- text message roles are user, assistant, and developer;
- top-level instructions are available;
- active response cancellation is available.

### Fixed constraints

These are treated as fixed constraints of the current local inference path:

- no persistent Responses conversation;
- no `previous_response_id` continuation;
- no explicit system-role message item;
- no non-streaming HTTP inference.

### Empirical capabilities

The current named probe matrix is:

- `baseline`;
- `image_input`;
- `file_input`;
- `function_tools`;
- `additional_tools`;
- `web_search`;
- `reasoning`;
- `verbosity`;
- `structured_output`.

Each result retains its sanitized classification and safe diagnostic fields.
The contract does not silently translate a generic rejection into
"unsupported."

## Contract states

### ready

`ready` requires all of the following:

- model scope exists;
- SIWC profile scope exists;
- probe timestamp exists;
- every expected probe is present;
- every expected probe is classified `supported` or
  `unsupported_route`;
- no probe remains merely `rejected`, `error`, `not_run`, or
  `model_unavailable`.

A `ready` contract may be consumed only when
`ready_for(current_profile, current_model)` is true.

### needs_review

`needs_review` means the full matrix reached terminal responses but one or
more probes are only `rejected`.

That is deliberately not equivalent to unsupported. A rejection can mean that
the experimental request shape is wrong or stale. Engineering must inspect the
safe diagnostics and current route schema, repair the probe when justified, and
rerun it.

This is engineering-owned probe work. It must not be converted into repetitive
principal-operated terminal QA.

### incomplete

`incomplete` means the contract cannot yet be frozen. Examples include:

- missing profile/model/timestamp scope;
- missing probes;
- baseline failure causing later probes to be `not_run`;
- bridge/runtime errors;
- model unavailable;
- other nonterminal evidence.

Higher-level empirical capability consumers remain blocked.

## Derivation lifecycle

After an in-app probe run:

1. Chatarium writes the sanitized probe report.
2. Chatarium stamps the report with the run time.
3. Chatarium derives the Local Inference Contract.
4. Diagnostics reports the resulting contract state.

On startup, a valid saved probe report also regenerates the contract
deterministically. This lets older evidence gain the contract artifact after an
upgrade without requiring a rerun merely because the derived file did not yet
exist.

Both the probe report and derived contract are included in Chatarium local
archive backup/restore.

## Freeze gate

The local-first sequencing rule is now:

`LOCAL CONVERSATIONS → CAPABILITY EVIDENCE → READY LOCAL INFERENCE CONTRACT → CONTEXT COMPOSER → BEHAVIOR PROFILE → LIFECYCLE STATE → ROUTING/MEMORY → CONTROLLER↔WORKERS → MCP/LOCAL TOOLS`

The contract substrate may be implemented and tested before empirical evidence
exists. Higher-level code may also be designed around the typed contract
interface. But it must not assume an empirical remote capability is usable
until the active profile/model contract is `ready`.

## Relationship to the human capability ledger

[LOCAL_INFERENCE_CAPABILITY_SURFACE.md](LOCAL_INFERENCE_CAPABILITY_SURFACE.md)
remains the human-readable inventory, rationale, and provenance ledger.

The machine-readable contract is narrower. It exists so executable behavior can
consume a reviewed capability boundary without scraping Markdown or inferring
from prose.

If the two appear to disagree, stop and reconcile the evidence/derivation. Do
not silently choose whichever source enables more functionality.
