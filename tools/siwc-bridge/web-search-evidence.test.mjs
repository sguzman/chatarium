import test from "node:test";
import assert from "node:assert/strict";
import { observeWebSearchCompletion, webSearchCompletedEvent } from "./web-search-evidence.mjs";

const sse = (events) => events.map((e) => "data: " + JSON.stringify(e) + "\n\n").join("");
function streamed(data, chunkSize = 17) {
  const bytes = new TextEncoder().encode(data);
  return new Response(new ReadableStream({
    start(controller) {
      for (let i = 0; i < bytes.length; i += chunkSize)
        controller.enqueue(bytes.slice(i, i + chunkSize));
      controller.close();
    },
  }), { status: 200, headers: { "content-type": "text/event-stream" } });
}

test("real completed web-search SSE is accepted even when chunk boundaries split JSON", async () => {
  const response = streamed(sse([
    { type: "response.web_search_call.searching", item_id: "ws_1" },
    { type: "response.web_search_call.completed", item_id: "ws_1" },
    { type: "response.completed", response: { output: [] } },
  ]), 7);
  assert.equal(await observeWebSearchCompletion(response), true);
  assert.equal((await response.text()).includes("web_search_call.completed"), true);
});

test("claiming web search in model text does not satisfy the evidence gate", async () => {
  const response = streamed(sse([
    { type: "response.output_text.delta", delta: "Yes, I can search the web." },
    { type: "response.completed", response: { output: [{ type: "message" }] } },
  ]));
  assert.equal(await observeWebSearchCompletion(response), false);
});

test("only completed web-search output items count; in-progress and failed do not", async () => {
  assert.equal(webSearchCompletedEvent({ type: "response.output_item.done", item: { type: "web_search_call", status: "completed" } }), true);
  assert.equal(webSearchCompletedEvent({ type: "response.output_item.done", item: { type: "web_search_call", status: "failed" } }), false);
  assert.equal(webSearchCompletedEvent({ type: "response.web_search_call.searching" }), false);
  assert.equal(await observeWebSearchCompletion(streamed(sse([
    { type: "response.output_item.done", item: { type: "web_search_call", status: "completed" } },
  ]))), true);
  assert.equal(await observeWebSearchCompletion(streamed(sse([
    { type: "response.completed", response: { output: [{ type: "web_search_call", status: "completed" }] } },
  ]))), true);
});

test("HTTP failures, malformed evidence, and unbounded lines fail closed", async () => {
  assert.equal(await observeWebSearchCompletion(new Response("failure", { status: 403 })), false);
  assert.equal(await observeWebSearchCompletion(streamed("data: {broken}\n")), false);
  assert.equal(await observeWebSearchCompletion(streamed("data: " + "x".repeat(130000))), false);
});
