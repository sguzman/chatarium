// A deterministic, credential-free DevKit protocol double.
// The production bridge is unchanged; this module is injected only by
// the explicit offline test's Node --import preload.
const connected = {
  status: "connected",
  sharing: true,
  profileId: "offline-fixture",
  profileLabel: "Synthetic QA",
};
const disconnected = { status: "disconnected", sharing: false };
let session = connected;
let onSession = () => {};

export function createChatGPT(config) {
  return {
    subscribe(observer) {
      onSession = observer;
      observer(session);
      return () => { onSession = () => {}; };
    },
    async getSession() { return session; },
    async signIn() {
      // Only a synthetic sample, never an OAuth credential.
      const sample = "OFFLINE_CRYPTO_ROUND_TRIP_ONLY";
      const encrypted = await config.credentialEncryption.encrypt(sample);
      if ((await config.credentialEncryption.decrypt(encrypted)) !== sample) {
        throw new Error("Offline encryption round trip failed");
      }
      session = connected;
      onSession(session);
      return session;
    },
    // Mirrors the pinned DevKit method. A cancel before signIn starts must
    // be a harmless no-op; the bridge is responsible for disarming preflight.
    cancelSignIn() {},
    async listModels() {
      return [{ slug: "offline-alpha-model", displayName: "Offline Alpha Model" }];
    },
    async disconnect() {
      session = disconnected;
      onSession(session);
    },
    async streamResponse({ model, input, instructions, signal, onDelta }) {
      if (session.status !== "connected") {
        const error = new Error("Offline fixture is disconnected");
        error.code = "not_connected";
        throw error;
      }
      const response = await fetch("https://api.openai.com/v1/responses", {
        method: "POST",
        body: JSON.stringify({
          model,
          input,
          instructions,
          stream: true,
          store: false,
        }),
      });
      const wire = await response.json();
      const text = "OFFLINE:" + JSON.stringify(wire);
      onDelta("OFFLINE:");
      if (input === "hold-for-cancel") {
        await new Promise((resolve, reject) => {
          if (signal.aborted) {
            const error = new Error("Synthetic stream aborted");
            error.code = "aborted";
            reject(error);
            return;
          }
          signal.addEventListener("abort", () => {
            const error = new Error("Synthetic stream aborted");
            error.code = "aborted";
            reject(error);
          }, { once: true });
        });
      }
      onDelta(JSON.stringify(wire));
      return { text };
    },
  };
}
