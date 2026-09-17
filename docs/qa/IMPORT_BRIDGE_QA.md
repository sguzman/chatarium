# Import bridge QA postmortem

The original manual browser-export -> native-journal QA was executed once on 2026-09-17 and is **retired**. It was too burdensome for the operator and exposed defects that are now covered by automated regression tests.

Do not reuse the old multi-step checklist.

## What the first real run proved

The first real flight-recorder v3 export established that:

- the browser export could be parsed and content-addressed;
- semantic native events could be appended to the journal;
- a second import of the exact same export appended zero duplicates;
- imported browser-scoped events did not hijack the native unscoped composer;
- human-readable imported message text rendered in the desktop surface.

## Defects exposed

The same run exposed three important problems.

### 1. Missing explicit destination silently contaminated normal state

The temporary PowerShell variable holding the QA destination did not survive the way the command block was invoked. The importer accepted the missing destination and silently fell back to the default `%LOCALAPPDATA%\Chatarium` directory.

**Resolution:** the importer now requires an explicit destination directory and refuses to run without one. Diagnostic/import tooling must not silently guess a durable destination.

### 2. Unsent draft capture was not reliable enough

The expected synthetic unsent draft was absent from the synchronous draft WAL. A prior incidental draft fragment was present instead.

**Resolution:** browser recorder v0.4 uses redundant draft capture: direct input observation, periodic composer polling, pre-send snapshots, navigation/offline snapshots, and an immediate synchronous snapshot before export.

### 3. Transient ChatGPT UI identity leaked into semantic history

The first turn was observed under both a temporary `conversation:WEB:...` identity and a later canonical conversation UUID. A `request-placeholder...` assistant node containing `Thinking` was also imported as if it were assistant content.

**Resolution:** importer regression logic canonicalizes duplicate observations around stable observed message IDs, prefers final conversation identities over temporary `WEB:` identities, collapses duplicate transcript observations, and records request placeholders as `assistant_status_observed` rather than assistant content.

## Current regression contract

CI should cover the mechanical properties that the operator previously had to prove manually:

- import idempotency;
- explicit destination requirement;
- conversation scope preservation/canonicalization;
- duplicate transcript collapse;
- placeholder/status classification;
- browser recorder syntax;
- native journal compatibility.

## Future human QA

Human involvement should be limited to authenticated browser behavior that cannot yet be simulated locally. The expected pattern is one short interaction and one generated artifact. Setup, hashing, importing, diffing, logs, and validation should be automated by Chatarium tooling.

See [`../HUMAN_QA.md`](../HUMAN_QA.md) for the automation-first QA policy.
