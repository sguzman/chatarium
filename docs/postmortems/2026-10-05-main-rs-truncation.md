# Postmortem: accidental whole-file truncation during SIWC diagnostics edit

**Incident date:** 2026-10-05  
**Scope:** remote GitHub edit of `apps/desktop/src/main.rs`  
**Status:** repaired; prevention rule added

## Executive summary

While adding self-diagnosing precondition text to the in-app SIWC capability-probe button, an engineering-side JavaScript replacement helper was called with the wrong argument shape.

The helper captured the mutable `main.rs` content through closure state, but the final test-insertion call accidentally passed the entire file content as the search argument. That call replaced the whole file with a 46-byte fragment beginning with `fn imported_event`.

Commit `1eaac6a29d9c9accaf5fc518316af257f5392efe` therefore truncated `apps/desktop/src/main.rs`. CI correctly failed immediately with an unclosed-delimiter error.

The failure was diagnosed from the earliest observable precondition: the file itself was malformed. Reading the committed file back showed a length of 46 bytes, confirming truncation before any runtime behavior was relevant.

The file was restored from the known-good parent `0b2171c09d5107d8a6c97f981645a35742c61e62`, the intended blocker-diagnostics change was reapplied using a pure `replaceOne(content, from, to, label)` transformation, and structural guards checked the reconstructed file length and required sentinels before commit.

The repaired implementation landed at `2d261af01a0955deca74cf32f5dc102653266057` and passed the full Windows and Linux CI suites.

## Product target

The intended product change was small:

- keep **RUN CAPABILITY PROBES** disabled when its prerequisites are not satisfied;
- explain the earliest failed prerequisite directly in Diagnostics;
- preserve the existing in-app SIWC evidence/contract workflow;
- add a regression test for blocker precedence.

No product behavior required replacing or rewriting the rest of `main.rs`.

## What happened

### 1. A stateful replacement helper was used

The edit script defined a helper that mutated a closure-owned `c` string rather than returning a transformed string.

Earlier replacement calls used that helper correctly.

### 2. The final call used the wrong signature

The test-insertion call was written as though the helper accepted `content` as its first parameter.

Because it did not, the current full file content became the search string and the intended insertion anchor became the replacement text.

The result was deterministic whole-file truncation.

### 3. Remote update success was mistaken for edit integrity

GitHub accepted the update because the payload was valid text and the blob SHA matched the current file.

The remote API succeeding proved only that a new blob was committed. It did not prove that the blob still represented a plausible `main.rs`.

### 4. CI exposed the corruption

Both rustfmt and Linux compilation reported an unclosed delimiter.

A direct readback then established the real state:

- committed `main.rs` length: 46 bytes;
- expected order of magnitude: hundreds of kilobytes.

That was the decisive failure evidence.

## Operator burden

The principal performed no manual QA and was not asked to repair or inspect the repository.

The cost was engineering time and CI churn rather than operator labor.

That is still a reliability failure because a destructive remote edit reached `main` before its output integrity was verified.

## Root causes

### Root cause A — mutation helper API was easy to misuse

The helper's signature did not match the normal pure-transform mental model.

A call-site mistake therefore changed the semantic meaning of every argument without producing a JavaScript exception.

### Root cause B — no pre-commit whole-file integrity guard

The update path checked replacement match counts but did not check final file size or required sentinels before sending the full replacement blob.

### Root cause C — no immediate post-commit readback

The commit SHA was accepted as sufficient evidence that the edit had landed correctly.

A single readback of file length and known markers would have detected the truncation before CI.

## Corrective implementation

The repair process:

1. fetched `main.rs` from the known-good parent commit;
2. used a pure replacement helper that explicitly receives and returns content;
3. reapplied only the intended probe-gating changes;
4. asserted the reconstructed file remained larger than 200 KB;
5. asserted an existing sentinel, `COPY INFERENCE CONTRACT`, remained present;
6. asserted the new blocker UI sentinel was present;
7. committed the restored full file;
8. ran the full Windows/Linux CI suite.

The repaired UI now reports the first blocker in this order:

1. ChatGPT plan connection not ready;
2. no account-visible model selected;
3. another response pending/active;
4. capability probe suite already running.

A unit test preserves that precedence.

## Permanent prevention rules

1. Treat any whole-file remote replacement as a destructive operation.
2. Before commit, verify transformed output has a plausible size relative to the source.
3. Before commit, verify important pre-existing structural sentinels remain present.
4. Before commit, verify the intended new sentinel is present.
5. After commit, immediately read the remote file back and verify size/sentinels again.
6. Prefer pure `replaceOne(content, from, to, label) -> content` transforms over helpers that mutate closure-owned file state.
7. A successful GitHub update response is not proof of semantic file integrity.
8. If CI reports a top-level parse failure after a whole-file edit, inspect committed file integrity before debugging compiler/runtime semantics.

## Reusable outcome

The intended Diagnostics improvement remains valid and is fully tested.

The incident adds no new uncertainty to the SIWC capability boundary itself. The project remains blocked only on the real in-app empirical probe run, not on this edit failure.
