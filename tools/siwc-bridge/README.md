# Chatarium Sign in with ChatGPT bridge

This local bridge connects the Rust desktop client to OpenAI's official
**Sign in with ChatGPT DevKit**.

The DevKit itself is pinned as the git submodule at
`vendor/openai-sign-in-with-chatgpt-devkit`. Chatarium does not copy or
relicense the DevKit source. The pinned upstream code remains subject to its own
license.

The bridge is deliberately credential-blind at its Rust boundary. It exposes
newline-delimited JSON commands for session state, sign-in, profile selection,
model discovery, disconnection, and streamed Responses API inference. Access,
refresh, and ID tokens never appear in bridge stdout.

## Bootstrap

One-time development bootstrap:

```sh
node tools/siwc-bridge/bootstrap.mjs
```

The bootstrap initializes the pinned submodule when necessary and builds only
the DevKit's `@siwc/local` package.

## Linux credential protection

The current Linux alpha encrypts the DevKit credential state with AES-256-GCM.
The encryption key is stored in the desktop Secret Service through
`secret-tool`; it is not stored in Chatarium's journal or bridge files. If the
Secret Service is unavailable, credential persistence fails closed.

This is intentionally separate from Chatarium's append-only conversation
journal.
