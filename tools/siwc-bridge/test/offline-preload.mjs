// Offline test-only Node preload. This is never imported by production.
// It intercepts precisely the pinned local DevKit module, not Chatarium logic.
import { registerHooks } from "node:module";

const replacement = new URL("./fake-devkit.mjs", import.meta.url).href;
registerHooks({
  resolve(specifier, context, nextResolve) {
    if (
      specifier.startsWith("file:") &&
      specifier.endsWith(
        "/vendor/openai-sign-in-with-chatgpt-devkit/packages/local/dist/index.js",
      )
    ) {
      return { url: replacement, shortCircuit: true };
    }
    return nextResolve(specifier, context);
  },
});

// Abort on any network route other than the synthetic Responses call.
// The real bridge applies its own one-shot request patches to this stub.
globalThis.fetch = async (input, init) => {
  if (String(input) !== "https://api.openai.com/v1/responses") {
    throw new Error("Offline alpha fixture blocked an unexpected network request");
  }
  if (init?.method !== "POST" || typeof init.body !== "string") {
    throw new Error("Offline alpha fixture rejected an unexpected request shape");
  }
  const body = JSON.parse(init.body);
  if (body.input === "simulate-ambiguous-network-loss") {
    // The POST has been attempted but no HTTP response is observed.
    // Retrying it would risk duplicating a generated turn.
    const error = new Error("synthetic response lost after request dispatch");
    error.code = "ECONNRESET";
    throw error;
  }
  const summary = {
    model: body.model,
    input: body.input,
    reasoning: body.reasoning ?? null,
    text: body.text ?? null,
    tools: body.tools ?? null,
    store: body.store,
    stream: body.stream,
  };
  return new Response(JSON.stringify(summary), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
};
