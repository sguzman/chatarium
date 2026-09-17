# QA: flight-recorder export -> native journal

This is the first human QA handoff for Chatarium. It deliberately uses a disposable ChatGPT conversation and an isolated temporary Chatarium data directory.

## Goal

Prove that a real browser flight-recorder v3 export can be:

1. exported from `chatgpt.com`;
2. imported into Chatarium without modifying the export;
3. archived by SHA-256;
4. translated into conversation-scoped native events;
5. imported a second time without duplicate events;
6. opened by Chatarium Desktop without an imported browser draft hijacking the native composer.

This test is **not** yet a protocol/HAR capture.

## Install / run

No new software should be installed for this QA if all of these already exist:

- Git;
- a Rust toolchain with `cargo`;
- Microsoft Edge or Chrome;
- Tampermonkey.

Before installing anything, verify from PowerShell:

```powershell
git --version
cargo --version
```

If either command is missing, stop and report which one is missing. Do not improvise an installation during this QA.

The Chatarium flight recorder must be installed as a Tampermonkey userscript from:

```text
browser/flight-recorder.user.js
```

in the checked-out Chatarium repository. Do not install a similarly named third-party script.

## Surfaces being touched

This QA touches only:

- the local `sguzman/chatarium` checkout;
- Tampermonkey's editor for the Chatarium flight-recorder userscript;
- one disposable/new `chatgpt.com` conversation;
- the browser Downloads folder for one JSON export;
- one newly-created directory under `%TEMP%`;
- Chatarium's importer and desktop binaries built by Cargo.

It does **not** require DevTools Network, HAR files, cookies, browser storage clearing, or the real/default `%LOCALAPPDATA%\Chatarium` data directory.

## Preconditions

- Pull the latest `main` before testing.
- Use a disposable ChatGPT conversation containing only the synthetic markers below.
- Close any running Chatarium Desktop process before importing.
- Do not set a persistent `CHATARIUM_DATA_DIR` environment variable. This procedure sets it only in the current PowerShell process when launching Desktop.
- Keep the exported JSON private even though the test conversation is synthetic.

## Exact actions

### A. Update the local repository

Open PowerShell in the existing Chatarium checkout and run:

```powershell
git switch main
git pull --ff-only
```

Then confirm the checkout includes the importer:

```powershell
Test-Path .\tools\importer\Cargo.toml
```

Expected output:

```text
True
```

### B. Install/update the Chatarium browser flight recorder

Open:

```text
browser/flight-recorder.user.js
```

from the local checkout.

In Tampermonkey:

1. Open the Chatarium flight-recorder userscript if it already exists; otherwise create one new userscript.
2. Replace the script body with the complete contents of the repository file.
3. Save it.
4. Reload `https://chatgpt.com/`.
5. Confirm the small **Chatarium** panel appears in the lower-right corner.

Do not alter the script during this QA.

### C. Create controlled browser evidence

Open a **new disposable ChatGPT conversation**.

Send exactly:

```text
respond with exactly CHATARIUM_IMPORT_TEST_001
```

Wait until an assistant response is visible. The ideal response is exactly `CHATARIUM_IMPORT_TEST_001`; if ChatGPT responds differently, do not retry solely to make it prettier. Record what actually happened.

Then type, but **do not send**:

```text
CHATARIUM_UNSENT_DRAFT_001
```

Wait until the Chatarium panel reports that the draft is saved.

Click **Export** in the Chatarium panel. This downloads a JSON file. Do not edit, rename internally, pretty-print, redact, or open-and-save the file before import.

Record its full filesystem path. In PowerShell, a convenient way to inspect recent candidate files is:

```powershell
Get-ChildItem "$HOME\Downloads" -Filter "chatarium-flight-recorder-*.json" |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 5 FullName, LastWriteTime
```

### D. Create an isolated QA data directory

In the same PowerShell window, create a unique temporary directory:

```powershell
$qa = Join-Path $env:TEMP ("chatarium-import-qa-" + [DateTimeOffset]::UtcNow.ToUnixTimeSeconds())
New-Item -ItemType Directory -Path $qa | Out-Null
$qa
```

Keep the printed path. This is the **only** Chatarium data directory used by this QA.

Set the export path explicitly, for example:

```powershell
$export = "C:\Users\YOUR_USER\Downloads\chatarium-flight-recorder-....json"
```

Use the actual full path from step C.

### E. First import

From the Chatarium repository root, with Chatarium Desktop closed, run:

```powershell
cargo run -p chatarium-importer -- flight-recorder $export $qa
```

Expected properties:

- command exits successfully;
- `source sha256:` is printed;
- `archive:` points under `$qa\imports\flight-recorder\`;
- `journal:` points to `$qa\journal.jsonl`;
- `planned semantic events:` is greater than zero;
- `appended this run:` is greater than zero;
- `already durable:` is zero.

Do not require a specific event count. The browser observer can legitimately capture more or fewer transcript observations depending on the current site DOM.

### F. Repeat the exact same import

Run the exact same command again without changing the export or QA directory:

```powershell
cargo run -p chatarium-importer -- flight-recorder $export $qa
```

Expected properties:

- command exits successfully;
- SHA-256 is identical to the first run;
- `appended this run:` is `0`;
- `already durable:` equals `planned semantic events:`.

This proves import idempotency against a real flight-recorder export.

### G. Open the native desktop against the isolated imported journal

Still in the same PowerShell process:

```powershell
$env:CHATARIUM_DATA_DIR = $qa
cargo run -p chatarium-desktop
```

Expected UI observations:

- Chatarium opens successfully;
- the **Composer is empty** even though the browser export contained `CHATARIUM_UNSENT_DRAFT_001`; imported browser drafts are scoped evidence and must not hijack the current native composer;
- under **Locally committed messages**, `respond with exactly CHATARIUM_IMPORT_TEST_001` is visible;
- that imported committed message shows a conversation scope instead of `native/unscoped`;
- expanding **Durable event journal** shows scoped imported events, including import boundary events and browser observations;
- imported text is rendered as human-readable text rather than a raw provenance JSON wall.

Do not press **Commit locally** during this QA. We are testing imported evidence, not creating new native events.

Close Chatarium Desktop after observing the result.

## Evidence to return

Send back exactly these three things:

1. the complete console output from the **first importer run**;
2. the complete console output from the **second importer run**;
3. one screenshot of Chatarium Desktop showing:
   - the imported committed test message and its scope;
   - the empty Composer;
   - enough of the event journal to show imported events exist.

Also say whether the assistant actually returned the requested exact marker or something else.

Do **not** send the raw JSON export unless specifically requested later. The importer output and screenshot should be enough for this QA.

## Do not touch

During this QA:

- do not clear `chatgpt.com` cookies, Local Storage, IndexedDB, or site data;
- do not export a private/personal conversation;
- do not edit the JSON export;
- do not import into `%LOCALAPPDATA%\Chatarium`;
- do not run Chatarium Desktop while the importer is writing the same journal;
- do not blindly resend if ChatGPT reports a timeout; keep the observed failure as evidence;
- do not send cookies, authorization headers, session tokens, or DevTools network dumps for this QA.

## Cleanup

After the result has been reviewed:

```powershell
$env:CHATARIUM_DATA_DIR = $null
```

Keep both the downloaded export and `$qa` directory until the QA result is accepted. After acceptance they can be deleted; neither belongs in Git.
