# Postmortem: unsynchronized SIWC capability-probe handoff

**Incident date:** 2026-10-05  
**Scope:** capability-probe execution handoff from repository work to the principal  
**Status:** closed with product and process corrections

## Executive summary

Chatarium implemented a credential-safe SIWC capability-probe harness in the remote GitHub repository, then immediately handed the principal a command that referenced the newly created local path:

`tools/siwc-bridge/capability-probe.mjs`

The handoff was invalid because there was no evidence that the principal's local checkout had synchronized to the commit containing that file. The command therefore failed at process startup with `MODULE_NOT_FOUND`.

Authentication was never reached.

The subsequent discussion initially focused on why an authenticated local SIWC session was needed for empirical route probing. That explanation was technically relevant to the eventual probe, but it was not the cause of the observed failure. The first failed precondition was simply that the requested file did not exist in the local checkout.

This created avoidable operator burden and violated Chatarium's existing zero-operator-regression-labor policy.

The corrective response was not to ask for another terminal command. The probe suite was integrated into the Chatarium Diagnostics UI, bound to the existing local SIWC session, given a fixed finite probe set, made non-overlapping with ordinary inference, and given sanitized durable report persistence.

## Product target

The target was:

> Establish empirical SIWC route capabilities without exposing OAuth credentials, widening the normal product API, or requiring the principal to operate developer plumbing.

The intended capability matrix included:

- baseline text admission;
- image input;
- file input;
- namespaced function/custom tools;
- `additional_tools`;
- web search;
- reasoning controls;
- text verbosity;
- structured output.

The probe had to preserve:

- credentials inside the trusted DevKit/bridge boundary;
- `store: false`;
- `stream: true`;
- the DevKit-selected model;
- a narrow request-patch allowlist;
- one Responses request at a time.

## What happened

### 1. Remote implementation completed

The developer-only probe harness was added to GitHub and committed to `main`.

The implementation itself was valid enough to pass syntax and bridge smoke checks.

### 2. The handoff incorrectly assumed local synchronization

The principal was then told to run the newly created script from the local repository.

No evidence had established that the local checkout contained the new commit.

This conflated two different states:

- remote repository state;
- the principal's current local checkout state.

The local process failed before any SIWC session lookup, request construction, network operation, or authentication-sensitive behavior occurred.

### 3. Diagnosis initially jumped to a later stage

The principal correctly questioned why authentication was involved.

The response explained that empirical SIWC capability probing ultimately requires the already-connected local ChatGPT session and that credentials should never be pasted or exported.

That security explanation was correct in isolation, but it did not diagnose the observed failure.

The observed failure was earlier:

> the executable path was absent locally.

The correct first response should have been to identify the local/remote state mismatch and stop issuing manual commands.

## Operator burden created

The incident forced the principal to:

- execute a command that could not succeed;
- surface the resulting `MODULE_NOT_FOUND`;
- challenge an irrelevant authentication explanation;
- spend attention distinguishing a file-synchronization failure from the later authenticated probe requirement.

None of this produced capability evidence.

This was pure avoidable operator labor.

## Root causes

### Root cause A — remote/local state conflation

A successful GitHub commit was implicitly treated as proof that the same file existed in the principal's local working tree.

That assumption was false.

### Root cause B — developer plumbing was handed to the operator

The probe harness was engineering infrastructure. It should not have been handed off as routine terminal QA when the same operation could be integrated into Chatarium.

### Root cause C — failure-stage discipline was weak

The diagnosis considered the eventual authenticated request path before establishing whether execution had even reached that path.

The first failed precondition must always be diagnosed first.

## What remains reusable

The following work was valid and remains in the project:

- the credential-safe internal `probe_response` bridge command;
- the narrow request-patch allowlist;
- normal-inference/probe mutual exclusion;
- the fixed capability matrix;
- sanitized error classification;
- the standalone developer CLI for engineering use;
- CI syntax and request-guard smoke coverage.

The incident was a handoff/process failure, not an invalidation of the probe mechanism.

## Corrective implementation

The probe workflow was moved into the desktop Diagnostics surface.

The current implementation:

- exposes a fixed **RUN CAPABILITY PROBES** action;
- uses the existing local ChatGPT sign-in;
- never asks the principal to provide a password, token, cookie, OAuth blob, or credential file;
- keeps credentials inside the SIWC DevKit/bridge;
- sends only the fixed finite probe definitions;
- blocks ordinary Send while a probe is active;
- persists sanitized results to `siwc-capability-probes.json`;
- reloads saved probe evidence across restarts;
- binds saved evidence to the DevKit's renderer-safe local `profileId`, selected model slug, and run timestamp;
- warns when a loaded report belongs to a different profile or model;
- includes the sanitized probe report in local archive backup/restore;
- discloses in the UI that probes use the existing local sign-in and consume small real plan-usage requests.

The implementation was validated on both Linux and Windows through commit
`3e3f9f32838f097829d2b5436146fb089f897aa5`.

## Permanent prevention rules

The following are now project rules:

1. A remote commit is not evidence that the principal's local checkout contains the changed file.
2. Never hand the principal a command referencing newly created remote code unless synchronization has been established.
3. Routine developer probes, smoke tests, and regression work belong to engineering-owned automation or in-app diagnostics, not the principal.
4. When a command fails, diagnose the earliest failed precondition before discussing later auth/network/runtime stages.
5. Never ask the principal to paste reusable credentials to bridge a missing automation surface.
6. If an explicit user action is retained because it intentionally consumes plan usage or exercises an account-scoped capability, the UI must make that consequence clear.

## Remaining unresolved item

The route capability results themselves are still empirical account/model evidence and cannot be inferred from the harness implementation.

The probe substrate is complete; the capability ledger should only be updated to SUPPORTED / REJECTED / ROUTE-UNSUPPORTED / MODEL-OR-ACCOUNT-CONSTRAINED after an actual in-app probe run produces evidence.
